//! The session transcript — the replayable half of the `/export` archive.
//!
//! [`super::export_session`] bundles nine telemetry *tables*: what the session
//! cost, which tools it called, which files it touched. None of that is the
//! session. The session is the ordered stream of what the model said, what it
//! ran, and what came back — and that lives in `events`, which the tables do
//! not summarize and the dumps do not contain.
//!
//! This module folds [`Store::session_events`](stella_store::Store::session_events)
//! into that stream and renders it as the archive's readable half.
//!
//! # Four properties this fold has to get right
//!
//! **1. It is a third egress sink, and #817's masking did not cover it.**
//! `redact_dump` runs over the table dumps, and the dashboard embeds those
//! same redacted dumps — two sinks, one masking pass. The event journal is
//! read straight from the store and reaches neither. Prompts, tool arguments
//! and tool output are exactly the places #817 masks credentials out of, so
//! **every string this module takes off an event goes through
//! [`redact`](stella_core::redact::redact_secrets) before it is escaped** (see
//! [`Fold::clean`]). An unredacted transcript would have quietly reopened #817 in an
//! artifact whose whole purpose is to be mailed to someone.
//!
//! **2. The elapsed clock has one-second resolution and may not pretend
//! otherwise.** `events.ts` takes SQLite's `DEFAULT CURRENT_TIMESTAMP` at every
//! writer, which carries no sub-second component
//! ([`SessionEventRecord::ts`](stella_store::SessionEventRecord::ts)). So the
//! `.t` column prints whole seconds. Per-call durations are *not* derived from
//! it — `ToolResult::duration_ms` and `StepUsage::duration_ms` are measured,
//! millisecond-precise, and printed as they are. A tenth of a second in the
//! elapsed column would be a measurement artifact invented by the renderer.
//!
//! **3. One model message renders once.** The engine streams prose as
//! [`AgentEvent::TextDelta`] fragments and then emits one consolidated
//! [`AgentEvent::Text`] carrying the same prose. A renderer that coalesces the
//! delta run *and* renders the `Text` event draws every assistant message
//! twice — and the duplicate looks like a second paid model call to anyone
//! reading the artifact rather than the stream. `Text`'s own protocol doc
//! settles it: consumers "must REPLACE any accumulated preview with this
//! value, never append". [`Fold::flush_prose`] implements exactly that, so a
//! delta run followed by its `Text` emits one row.
//!
//! **4. A tool result knows neither its name nor its arguments.**
//! `ToolResult` carries `call_id`, a typed [`ToolOutput`] and a duration —
//! the name and the arguments are on the `ToolStart` sharing that `call_id`.
//! The correlation map is built across the whole journal, never per batch.
//! Failure is read off the `ToolOutput::Error` tag, never sniffed out of the
//! text.

use std::collections::HashMap;
use std::fmt::Write as _;

use serde::Serialize;
use stella_protocol::{AgentEvent, ToolOutput};
use stella_store::{SessionEventRecord, SessionJournal};

/// Cap on a single embedded tool result, file diff, or structured payload.
///
/// The archive is a file someone opens in a browser; one `bash` call that cats
/// a build log can carry tens of megabytes, and a handful of them make the
/// dashboard slower to open than the run took to execute. The cut is stated
/// inline (`… N bytes truncated`) rather than silent — an elided transcript
/// that looks complete is worse evidence than a short one that says so — and
/// the untruncated rows remain in `raw/` beside it.
const MAX_EMBEDDED_BYTES: usize = 64 * 1024;

/// Cap on the number of rendered rows.
///
/// Same reasoning one level up: a long-running session can hold six figures of
/// events, and past a point the page stops being readable at all. Overflow is
/// reported in the transcript header, never dropped quietly.
const MAX_ROWS: usize = 5_000;

/// One rendered transcript row, already escaped and ready to concatenate.
struct Row {
    html: String,
}

/// The rendered transcript plus what the render had to leave out.
pub(crate) struct Transcript {
    /// The `<div class="ev …">` rows, in stream order.
    pub(crate) body: String,
    /// How many rows were rendered.
    pub(crate) rendered: usize,
    /// Events the store could not parse back into an `AgentEvent`
    /// ([`SessionJournal::skipped`]) — a stream recorded before a variant
    /// existed. Stated so a reader can tell a quiet session from a lossy read.
    pub(crate) unparseable: usize,
    /// Rows dropped by [`MAX_ROWS`].
    pub(crate) overflow: usize,
    /// Whether any string in the transcript had a credential masked out of it.
    pub(crate) redacted: bool,
}

impl Transcript {
    /// The one-line provenance note the transcript panel prints under its
    /// heading — what this rendering does and does not contain.
    pub(crate) fn provenance(&self) -> String {
        let mut parts = vec![format!("{} entries", comma_usize(self.rendered))];
        if self.overflow > 0 {
            parts.push(format!(
                "{} further entries not rendered (the archive's <code>raw/</code> \
                 dumps are complete)",
                comma_usize(self.overflow)
            ));
        }
        if self.unparseable > 0 {
            parts.push(format!(
                "{} event(s) could not be replayed (recorded by an older build)",
                comma_usize(self.unparseable)
            ));
        }
        if self.redacted {
            parts.push("credentials masked".to_string());
        }
        parts.join(" · ")
    }
}

/// Fold a session's journal into transcript rows.
///
/// `prompts` maps `execution_id` to that execution's prompt, so the transcript
/// opens each turn with what was actually asked. It comes from the already
/// redacted `executions` dump rather than from the journal, because the prompt
/// is a column, not an event.
pub(crate) fn render(journal: &SessionJournal, prompts: &HashMap<i64, String>) -> Transcript {
    let mut fold = Fold::new(prompts);
    for record in &journal.events {
        fold.push(record);
    }
    fold.finish(journal.skipped)
}

/// Accumulator for [`render`].
struct Fold<'a> {
    rows: Vec<Row>,
    prompts: &'a HashMap<i64, String>,
    /// Epoch seconds of the first event that carried a parseable timestamp —
    /// the origin the `.t` column counts from.
    origin: Option<i64>,
    /// Elapsed seconds for the row currently being built.
    now: i64,
    /// The execution whose prompt has already been printed.
    current_execution: Option<i64>,
    /// Accumulated `TextDelta` fragments awaiting either their consolidated
    /// `Text` (which replaces them) or any other event (which flushes them).
    pending_text: String,
    /// Accumulated `Reasoning` fragments. Unlike prose these have no
    /// consolidated counterpart, so a run always flushes to a row.
    pending_reasoning: String,
    /// `call_id` → the index in `rows` of the tool row awaiting its result.
    open_tools: HashMap<String, usize>,
    /// `call_id` → (tool name, delegate id), for a result whose call was
    /// never recorded, or whose own `ToolResult` did not repeat the
    /// attribution its `ToolStart` already carried (#4699).
    tool_names: HashMap<String, (String, Option<String>)>,
    redacted: bool,
    overflow: usize,
}

impl<'a> Fold<'a> {
    fn new(prompts: &'a HashMap<i64, String>) -> Self {
        Self {
            rows: Vec::new(),
            prompts,
            origin: None,
            now: 0,
            current_execution: None,
            pending_text: String::new(),
            pending_reasoning: String::new(),
            open_tools: HashMap::new(),
            tool_names: HashMap::new(),
            redacted: false,
            overflow: 0,
        }
    }

    /// Absorb one journal record.
    fn push(&mut self, record: &SessionEventRecord) {
        self.now = self.elapsed(&record.ts);

        // A `Text` event REPLACES the delta run it consolidates (see the
        // module doc, property 3), so it must not flush the buffer first —
        // every other event must.
        if !matches!(
            record.event,
            AgentEvent::TextDelta { .. } | AgentEvent::Text { .. }
        ) {
            self.flush_prose();
        }
        if !matches!(record.event, AgentEvent::Reasoning { .. }) {
            self.flush_reasoning();
        }

        self.open_execution(record.execution_id);
        self.event(&record.event);
    }

    /// Elapsed whole seconds since the first timestamped event.
    ///
    /// An unparseable or absent `ts` holds the previous reading rather than
    /// resetting to zero: the row's position in the stream is authoritative
    /// and a jump to `0.0s` mid-transcript would read as a new run.
    fn elapsed(&mut self, ts: &str) -> i64 {
        let Some(secs) = parse_sql_timestamp(ts) else {
            return self.now;
        };
        match self.origin {
            Some(origin) => (secs - origin).max(0),
            None => {
                self.origin = Some(secs);
                0
            }
        }
    }

    /// Open a new turn when the execution changes, printing its prompt.
    fn open_execution(&mut self, execution_id: i64) {
        if self.current_execution == Some(execution_id) {
            return;
        }
        self.current_execution = Some(execution_id);
        let Some(prompt) = self.prompts.get(&execution_id) else {
            return;
        };
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return;
        }
        // Already redacted: this came off the `executions` dump, which
        // `redact_dump` has run over. `clean` again anyway — masking twice is
        // free and a change of source upstream must not silently unmask.
        let prose = self.clean(prompt);
        self.prose_row("user", "USER", &prose);
    }

    /// Render one event.
    fn event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Stage { name, .. } => {
                let name = wire_token(name);
                self.row(format!(
                    r#"<div class="ev stage">{t}<span class="lbl">STAGE</span><b>{name}</b></div>"#,
                    t = self.stamp(),
                    name = escape(&name),
                ));
            }

            // Prose. The delta run is a preview of the `Text` that follows it;
            // whichever arrives is rendered, never both.
            AgentEvent::TextDelta { delta } => self.pending_text.push_str(delta),
            AgentEvent::Text { text } => {
                self.pending_text.clear();
                let prose = self.clean(text);
                if !prose.trim().is_empty() {
                    self.prose_row("say", "ASSISTANT", &prose);
                }
            }
            AgentEvent::Reasoning { delta } => self.pending_reasoning.push_str(delta),

            AgentEvent::StepUsage {
                step,
                role,
                provider,
                model,
                input_tokens,
                output_tokens,
                cached_input_tokens,
                cost_usd,
                duration_ms,
                tool_calls,
                finish_reason,
                ..
            } => {
                let role = wire_token(role);
                let route = if provider.is_empty() {
                    model.clone()
                } else {
                    format!("{provider}/{model}")
                };
                let finish = finish_reason
                    .as_ref()
                    .map_or_else(|| "-".to_string(), wire_token);
                let meta = format!(
                    "out {out} tok · in {input} (cached {cached}) · ${cost:.4} · {secs:.1}s · \
                     finish={finish} · tools={tools}",
                    out = comma_u64(*output_tokens),
                    input = comma_u64(*input_tokens),
                    cached = comma_u64(*cached_input_tokens),
                    cost = cost_usd,
                    secs = *duration_ms as f64 / 1000.0,
                    finish = escape(&finish),
                    tools = tool_calls,
                );
                self.row(format!(
                    r#"<div class="ev step" data-role="{role_attr}">{t}<span class="lbl">STEP</span><b>step {step} · {role} · {route}</b><div class="meta">{meta}</div></div>"#,
                    role_attr = escape(&role),
                    t = self.stamp(),
                    role = escape(&role),
                    route = escape(&route),
                ));
            }

            AgentEvent::ToolStart { call, sub_agent_id } => {
                let args = self.clean(&pretty(&call.input));
                let name = self.clean(&call.name);
                self.tool_names
                    .insert(call.call_id.clone(), (name.clone(), sub_agent_id.clone()));
                let index = self.rows.len();
                let stamp = self.stamp();
                let agent = agent_suffix(sub_agent_id.as_deref());
                // The result arrives later in the stream and is patched into
                // this row; until then it is honestly marked pending, because
                // a killed run leaves calls that genuinely never returned.
                self.row(format!(
                    r#"<details class="ev tool" open><summary>{stamp}<span class="lbl">TOOL</span><b>{name}{agent}</b></summary><pre class="in">{args}</pre><pre class="out pending">(no result recorded — the run ended before this call returned)</pre></details>"#,
                    name = escape(&name),
                    args = escape(&clip(&args)),
                ));
                if self.rows.len() > index {
                    self.open_tools.insert(call.call_id.clone(), index);
                }
            }
            AgentEvent::ToolResult {
                call_id,
                output,
                duration_ms,
                speculated,
                sub_agent_id,
            } => self.close_tool(
                call_id,
                output,
                *duration_ms,
                *speculated,
                sub_agent_id.as_deref(),
            ),

            AgentEvent::FileChange {
                path,
                kind,
                added,
                removed,
                diff,
                ..
            } => {
                let kind = wire_token(kind);
                let path = self.clean(path);
                let diff = self.clean(diff.as_deref().unwrap_or_default());
                self.row(format!(
                    r#"<details class="ev file"><summary>{t}<span class="lbl">FILE</span><b>{kind} {path} (+{added}/-{removed})</b></summary>{body}</details>"#,
                    t = self.stamp(),
                    kind = escape(&kind),
                    path = escape(&path),
                    body = diff_html(&diff),
                ));
            }

            AgentEvent::Verdict { passed, evidence } => {
                let label = if *passed { "PASS" } else { "FAIL" };
                self.structured_row(
                    if *passed { "verdict" } else { "verdict err" },
                    "VERDICT",
                    &format!("verdict · {label}"),
                    evidence,
                );
            }
            AgentEvent::GoalVerdict {
                round,
                met,
                reasoning,
                cost_usd,
            } => {
                let prose = self.clean(reasoning);
                let title = format!(
                    "goal round {round} · {} · ${cost_usd:.4}",
                    if *met { "met" } else { "not met" }
                );
                self.note_row(
                    if *met { "verdict" } else { "verdict err" },
                    "GOAL",
                    &title,
                    Some(&prose),
                );
            }

            // The proof stream is the one place a full reason string survives:
            // every other surface truncates it, and the truncated half is
            // routinely the half that explains the run. An offline document has
            // no width budget, so it prints whole.
            AgentEvent::Proof { step } => self.structured_row("proof", "PROOF", "proof step", step),

            AgentEvent::Error { message, retryable } => {
                let prose = self.clean(message);
                let title = if *retryable {
                    "error (retryable)"
                } else {
                    "error"
                };
                self.note_row("err", "ERROR", title, Some(&prose));
            }
            AgentEvent::Retry { attempt, reason } => {
                let prose = self.clean(reason);
                self.note_row("err", "RETRY", &format!("attempt {attempt}"), Some(&prose));
            }
            AgentEvent::Steered { text, .. } => {
                let prose = self.clean(text);
                self.prose_row("user", "STEER", &prose);
            }
            AgentEvent::Commit { sha, message } => {
                let sha = self.clean(sha);
                let message = self.clean(message);
                let short = sha.chars().take(12).collect::<String>();
                self.note_row("note", "COMMIT", &short, Some(&message));
            }
            AgentEvent::TurnComplete { model, cost_usd } => {
                let model = self.clean(model);
                self.note_row(
                    "note",
                    "COMPLETE",
                    &format!("{model} · ${cost_usd:.4}"),
                    None,
                );
            }

            // Everything else in the ~40-variant stream. A transcript that
            // silently dropped the rest would be a summary claiming to be a
            // replay, so each one renders as its wire tag plus its complete
            // payload — legible without this module having to grow an opinion
            // about a variant it has never seen.
            other => {
                let tag = event_tag(other);
                self.structured_row("note", "EVENT", &tag, other);
            }
        }
    }

    /// Attach a result to the tool row its `call_id` opened.
    ///
    /// `sub_agent_id` is the result's own field — read only as the fallback
    /// for a call whose `ToolStart` was never recorded, since the row the
    /// common path patches already carries the announcement's badge (#4699).
    fn close_tool(
        &mut self,
        call_id: &str,
        output: &ToolOutput,
        duration_ms: u64,
        spec: bool,
        sub_agent_id: Option<&str>,
    ) {
        // The tag is the whole verdict — `ToolResult` has no `error` field and
        // never has, so branching on the enum is the only correct read.
        let failed = output.is_error();
        let body = match output {
            ToolOutput::Ok { content, .. } => content.clone(),
            ToolOutput::Error { message, .. } => message.clone(),
        };
        let body = self.clean(&body);
        let (name, started_agent) = self
            .tool_names
            .get(call_id)
            .cloned()
            .unwrap_or_else(|| ("tool".to_string(), None));
        let meta = format!(
            " · {:.2}s{}",
            duration_ms as f64 / 1000.0,
            if spec { " · speculative" } else { "" }
        );

        let Some(index) = self.open_tools.remove(call_id) else {
            // A result with no recorded call — an older stream, or a journal
            // read that began mid-turn. Render it rather than drop it.
            let agent = agent_suffix(started_agent.as_deref().or(sub_agent_id));
            self.row(format!(
                r#"<details class="ev tool{err} open"><summary>{t}<span class="lbl">TOOL</span><b>{name}{agent}</b><span class="meta">{meta}</span></summary><pre class="out{outerr}">{body}</pre></details>"#,
                err = if failed { " err" } else { "" },
                t = self.stamp(),
                name = escape(&name),
                meta = escape(&meta),
                outerr = if failed { " err" } else { "" },
                body = escape(&clip(&body)),
            ));
            return;
        };
        let Some(row) = self.rows.get_mut(index) else {
            return;
        };
        if failed {
            row.html = row
                .html
                .replacen(r#"class="ev tool""#, r#"class="ev tool err""#, 1);
        }
        row.html = row.html.replacen(
            "</b></summary>",
            &format!(
                "</b><span class=\"meta\">{}</span></summary>",
                escape(&meta)
            ),
            1,
        );
        row.html = row.html.replacen(
            r#"<pre class="out pending">(no result recorded — the run ended before this call returned)</pre>"#,
            &format!(
                r#"<pre class="out{err}">{body}</pre>"#,
                err = if failed { " err" } else { "" },
                body = escape(&clip(&body)),
            ),
            1,
        );
    }

    /// Flush an accumulated `TextDelta` run that never received its
    /// consolidating `Text` — a stream cut short mid-message.
    fn flush_prose(&mut self) {
        if self.pending_text.trim().is_empty() {
            self.pending_text.clear();
            return;
        }
        let text = std::mem::take(&mut self.pending_text);
        let prose = self.clean(&text);
        self.prose_row("say", "ASSISTANT", &prose);
    }

    /// Flush an accumulated `Reasoning` run.
    fn flush_reasoning(&mut self) {
        if self.pending_reasoning.trim().is_empty() {
            self.pending_reasoning.clear();
            return;
        }
        let text = std::mem::take(&mut self.pending_reasoning);
        let prose = self.clean(&text);
        self.prose_row("think", "THINKING", &prose);
    }

    /// A prose row (`user` / `say` / `think`).
    fn prose_row(&mut self, class: &str, label: &str, prose: &str) {
        self.row(format!(
            r#"<div class="ev {class}">{t}<span class="lbl">{label}</span><div class="prose">{prose}</div></div>"#,
            t = self.stamp(),
            prose = escape(&clip(prose)),
        ));
    }

    /// A one-line row with an optional prose body.
    fn note_row(&mut self, class: &str, label: &str, title: &str, body: Option<&str>) {
        let meta = body
            .map(|b| format!(r#"<div class="meta">{}</div>"#, escape(&clip(b.trim()))))
            .unwrap_or_default();
        self.row(format!(
            r#"<div class="ev {class}">{t}<span class="lbl">{label}</span><b>{title}</b>{meta}</div>"#,
            t = self.stamp(),
            title = escape(title),
        ));
    }

    /// A row whose payload is a structured protocol value, rendered whole in a
    /// collapsible. See the `Proof` arm for why "whole" is the point.
    fn structured_row<T: Serialize>(&mut self, class: &str, label: &str, title: &str, value: &T) {
        let json = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into());
        let json = self.clean(&json);
        self.row(format!(
            r#"<details class="ev {class}"><summary>{t}<span class="lbl">{label}</span><b>{title}</b></summary><pre class="out">{json}</pre></details>"#,
            t = self.stamp(),
            title = escape(title),
            json = escape(&clip(&json)),
        ));
    }

    /// The `.t` timestamp cell. Whole seconds — see the module doc, property 2.
    fn stamp(&self) -> String {
        format!(r#"<span class="t">{:>6}</span>"#, format!("{}s", self.now))
    }

    /// Mask any credential in an event-derived string (module doc, property 1).
    fn clean(&mut self, raw: &str) -> String {
        let redaction = stella_core::redact::redact_secrets(raw);
        if redaction.redacted {
            self.redacted = true;
        }
        redaction.text
    }

    /// Append a row, respecting [`MAX_ROWS`].
    fn row(&mut self, html: String) {
        if self.rows.len() >= MAX_ROWS {
            self.overflow += 1;
            return;
        }
        self.rows.push(Row { html });
    }

    fn finish(mut self, unparseable: usize) -> Transcript {
        self.flush_prose();
        self.flush_reasoning();
        let mut body = String::new();
        for row in &self.rows {
            let _ = writeln!(body, "{}", row.html);
        }
        Transcript {
            body,
            rendered: self.rows.len(),
            unparseable,
            overflow: self.overflow,
            redacted: self.redacted,
        }
    }
}

/// The `type` tag an event serializes under — the same token the raw stream
/// carries, so a reader can grep `raw/` for a row they saw on the page.
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

/// A protocol enum's wire token (its `snake_case` serde name), so the page and
/// the raw dumps beside it use one vocabulary.
fn wire_token<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => "unknown".to_string(),
    }
}

/// Pretty-print tool arguments; fall back to the compact form.
fn pretty(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// Truncate to [`MAX_EMBEDDED_BYTES`], stating the cut.
///
/// Cuts on a character boundary — the byte budget is a size limit, and slicing
/// a UTF-8 sequence in half would panic on exactly the multi-byte output
/// (a tree-drawing tool, a non-English file) least likely to appear in a test.
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
        comma_usize(dropped)
    )
}

/// Escape a string for HTML markup. Mirrors [`super::escape_html`]; kept as a
/// one-line alias so every interpolation in this module reads the same.
fn escape(raw: &str) -> String {
    super::escape_html(raw)
}

/// The ` · agent d:1` suffix a tool row's label carries when a delegate made
/// the call — empty for the lead's own, which is the ordinary case (#4699).
/// Escaped here rather than through [`Fold::clean`]: the id is an opaque
/// handle the engine stamps, not model- or user-authored text, so it needs no
/// redaction — only the same HTML-escaping every other interpolation gets.
fn agent_suffix(sub_agent_id: Option<&str>) -> String {
    match sub_agent_id {
        Some(id) => format!(" · agent {}", escape(id)),
        None => String::new(),
    }
}

/// One file change's diff as the archive's `.dx` block: both line-number
/// gutters, a sigil column, a tinted row per change, and — within a modified
/// line pair — a second, stronger tint over the exact tokens that changed.
/// The same shape the Observatory draws and the deck draws, because this
/// archive is read side by side with them as evidence on a pull request.
///
/// Word-level highlighting is [`stella_transcript::word::highlight`], the
/// same LCS-over-tokens pass the transcript surfaces use, reused rather than
/// re-implemented: two copies of a diff tokenizer drift exactly the way two
/// copies of a diff renderer did before this module existed. It paints with
/// this page's own `--ok`/`--bad` tokens, not a new hue — this page's colour
/// system is closed to categorical hues until #3630 settles that question for
/// the transcript surfaces generally.
///
/// Only the changed lines and their context, never a file listing, and never
/// more than [`stella_diff::view::VIEW_CAP`] of them: the elided middle is
/// stated in place as a `⋯ n lines not shown` row. That cap replaces a byte
/// clip that cut mid-hunk, so a rewritten file no longer contributes 64 KiB
/// of `+` lines to a file someone is expected to open in a browser.
///
/// Diff text that parses to no hunks at all — a headerless pseudo-diff, or
/// something that is not a diff — falls back to preformatted text rather than
/// rendering nothing: an unrecognised shape must still be readable.
fn diff_html(diff: &str) -> String {
    if diff.trim().is_empty() {
        return String::new();
    }
    let view = stella_diff::view::elide(
        &stella_diff::parse::hunks(diff),
        stella_diff::view::VIEW_CAP,
    );
    if view.hunks.is_empty() {
        return format!(r#"<pre class="diff">{}</pre>"#, escape(&clip(diff)));
    }
    let fold = format!(
        r#"<div class="dx-fold">⋯ {} line{} not shown</div>"#,
        view.hidden,
        if view.hidden == 1 { "" } else { "s" }
    );
    let mut out = String::from(r#"<div class="dx">"#);
    for (i, hunk) in view.hunks.iter().enumerate() {
        if view.fold_before == Some(i) {
            out.push_str(&fold);
        }
        let _ = write!(
            out,
            r#"<div class="dx-hunk"><div class="dx-at">{}</div>"#,
            escape(&hunk.header())
        );
        // Line numbers are walked from this hunk's own header, which is why
        // `elide` re-headers a hunk it splits: the tail's gutter has to name
        // the file's real lines, not restart from the head's.
        let (mut old_no, mut new_no) = (hunk.old_start, hunk.new_start);
        diff_hunk_rows(&hunk.lines, &mut old_no, &mut new_no, &mut out);
        out.push_str("</div>");
    }
    if view.fold_before == Some(view.hunks.len()) {
        out.push_str(&fold);
    }
    out.push_str("</div>");
    out
}

/// Emit one hunk's rows, pairing each run of removals with the run of
/// additions that follows it — positional, the `k`th removal against the
/// `k`th addition — so a modified line gets word-level spans. Mirrors
/// `stella_transcript::file_diff::render_hunk`'s block-pairing exactly;
/// duplicated here rather than shared because that function consumes a
/// `FileChange`'s raw before/after text, and this page already holds a
/// parsed, elided `stella_diff::Hunk` with no text to rebuild it from.
fn diff_hunk_rows(
    lines: &[stella_diff::DiffLine],
    old_no: &mut usize,
    new_no: &mut usize,
    out: &mut String,
) {
    let mut i = 0;
    while i < lines.len() {
        match lines[i].op {
            stella_diff::Op::Equal => {
                diff_row(
                    " ",
                    "",
                    *old_no,
                    *new_no,
                    &[stella_transcript::word::Span {
                        text: lines[i].text.clone(),
                        changed: false,
                    }],
                    out,
                );
                *old_no += 1;
                *new_no += 1;
                i += 1;
            }
            stella_diff::Op::Remove | stella_diff::Op::Add => {
                let start = i;
                while i < lines.len() && lines[i].op == stella_diff::Op::Remove {
                    i += 1;
                }
                let removals = &lines[start..i];
                let add_start = i;
                while i < lines.len() && lines[i].op == stella_diff::Op::Add {
                    i += 1;
                }
                let additions = &lines[add_start..i];
                diff_change_block(removals, additions, old_no, new_no, out);
            }
        }
    }
}

/// One run of removals followed by its run of additions, word-highlighted
/// where they pair. See [`diff_hunk_rows`] for why this duplicates
/// `stella_transcript::file_diff::emit_change_block` rather than calling it.
fn diff_change_block(
    removals: &[stella_diff::DiffLine],
    additions: &[stella_diff::DiffLine],
    old_no: &mut usize,
    new_no: &mut usize,
    out: &mut String,
) {
    let paired = removals.len().min(additions.len());
    let mut old_spans = Vec::with_capacity(removals.len());
    let mut new_spans = Vec::with_capacity(additions.len());

    for k in 0..paired {
        let (o, n) = stella_transcript::word::highlight(&removals[k].text, &additions[k].text);
        old_spans.push(o);
        new_spans.push(n);
    }
    for line in &removals[paired..] {
        old_spans.push(vec![stella_transcript::word::Span {
            text: line.text.clone(),
            changed: false,
        }]);
    }
    for line in &additions[paired..] {
        new_spans.push(vec![stella_transcript::word::Span {
            text: line.text.clone(),
            changed: false,
        }]);
    }

    for spans in &old_spans {
        diff_row("−", " rem", *old_no, 0, spans, out);
        *old_no += 1;
    }
    for spans in &new_spans {
        diff_row("+", " add", 0, *new_no, spans, out);
        *new_no += 1;
    }
}

/// One `.dx-line` row. `old_no`/`new_no` of `0` renders as the empty gutter
/// cell — the same convention the old flat loop used, now driven by which
/// side actually has a line number rather than by `line.op` directly.
fn diff_row(
    sigil: &str,
    class: &str,
    old_no: usize,
    new_no: usize,
    spans: &[stella_transcript::word::Span],
    out: &mut String,
) {
    let old_cell = if old_no == 0 {
        String::new()
    } else {
        old_no.to_string()
    };
    let new_cell = if new_no == 0 {
        String::new()
    } else {
        new_no.to_string()
    };
    let _ = write!(
        out,
        r#"<div class="dx-line{class}"><span class="no">{old_cell}</span>"#,
    );
    let _ = write!(
        out,
        r#"<span class="nn">{new_cell}</span><span class="sg">{sigil}</span>"#,
    );
    out.push_str(r#"<span class="tx">"#);
    for span in spans {
        if span.changed {
            let _ = write!(out, r#"<span class="ww">{}</span>"#, escape(&span.text));
        } else {
            out.push_str(&escape(&span.text));
        }
    }
    out.push_str("</span></div>");
}

/// Parse `events.ts` (`YYYY-MM-DD HH:MM:SS`) to Unix seconds.
///
/// Positional, on the fixed width SQLite's `CURRENT_TIMESTAMP` writes; the
/// separator between date and time is not inspected, so an RFC-3339 `T` parses
/// identically. Anything shorter or non-numeric returns `None` and the caller
/// holds the previous reading — a wrong elapsed time is worse than a repeated
/// one, because the whole column is relative.
fn parse_sql_timestamp(text: &str) -> Option<i64> {
    if text.len() < 19 {
        return None;
    }
    let num = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01, by Howard Hinnant's `days_from_civil`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `1234567` → `1,234,567`.
fn comma_u64(n: u64) -> String {
    super::comma(n.min(i64::MAX as u64) as i64)
}

/// `1234567` → `1,234,567`.
fn comma_usize(n: usize) -> String {
    super::comma(n.min(i64::MAX as usize) as i64)
}

#[cfg(test)]
mod tests;
