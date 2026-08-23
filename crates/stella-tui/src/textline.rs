//! The shared event→text vocabulary — one lookup table for both rendering
//! surfaces (issue #66).
//!
//! Two independent renderers consume [`stella_protocol::AgentEvent`]s: the
//! plain `colored`+`println` surface in `stella-cli` (REPL and one-shot
//! modes) and this crate's ratatui transcript. Before this module each kept
//! its own event→string mapping, so every new `AgentEvent` variant had to be
//! worded twice. The contract now: **wording lives here, styling stays with
//! each surface.** A constructor per annotation variant yields an
//! [`EventLine`] of semantic pieces (glyph, tone, body, detail) that each
//! surface maps onto its own palette — `colored` codes on the plain surface,
//! `ratatui` styles on the deck.
//!
//! The wording is byte-load-bearing: the plain renderer's observable output
//! is composed as `"  {glyph} {body}"` (plus `" {detail}"` when present), and
//! the fixture tests at the bottom pin every line to the exact strings the
//! plain surface printed before the extraction. Change a string here and the
//! plain CLI's output changes with it — that is the point, but it must be
//! deliberate.
//!
//! Deliberately *not* here: streaming `Text`/`Reasoning` (accumulated, then
//! markdown-rendered or printed raw per surface), `Stage` transitions (the
//! deck draws rules, the plain surface prints only a "thinking…" cue), and
//! the `ToolStart`/`ToolResult` cards (the two surfaces present tool traffic
//! structurally differently — key=value cards vs an aligned label column —
//! and unifying them is a behavior change out of scope for #66).

use stella_protocol::{
    AgentEvent, BudgetMode, CiStatus, FileChangeKind, MediaJobState, MediaKind, PrStatus,
    ProviderShare, StageKind, StageName, TaskItem, TaskStatus,
};

/// Semantic weight of an annotation line. Each surface owns the mapping to
/// its palette (e.g. plain maps `Muted` to ANSI dim, the deck to
/// `theme::MUTED`); no color name may appear in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Success,
    Warn,
    Error,
    Muted,
}

/// One transcript annotation, split where the surfaces apply different
/// emphasis: a `glyph` carrying the line's `tone` (and `strong` for the few
/// glyphs both surfaces embolden), the `body` text, and an optional trailing
/// `detail` each surface de-emphasizes (dimmed / muted).
#[derive(Debug, Clone, PartialEq)]
pub struct EventLine {
    pub glyph: &'static str,
    pub tone: Tone,
    pub strong: bool,
    pub body: String,
    pub detail: Option<String>,
}

impl EventLine {
    /// The line's unstyled text, exactly as the plain surface prints it
    /// (minus its two-space indent) — what the fixture tests pin.
    pub fn text(&self) -> String {
        match &self.detail {
            Some(detail) => format!("{} {} {}", self.glyph, self.body, detail),
            None => format!("{} {}", self.glyph, self.body),
        }
    }
}

// ── Per-variant constructors: the one place each line is worded ─────────────

pub fn retry(attempt: u32, reason: &str) -> EventLine {
    EventLine {
        glyph: "↻",
        tone: Tone::Warn,
        strong: false,
        body: format!("retry #{attempt}:"),
        detail: Some(reason.to_string()),
    }
}

/// The park notice (#1857) — the turn is waiting on the engine's clock, with
/// no model calls until the watched state changes or the deadline expires.
pub fn parked(description: &str, poll_interval_secs: u64, deadline_secs: u64) -> EventLine {
    EventLine {
        glyph: "⏳",
        tone: Tone::Info,
        strong: true,
        body: format!(
            "parked until {description} — probing every {poll_interval_secs}s for up to \
             {deadline_secs}s, no model calls while waiting"
        ),
        detail: None,
    }
}

/// The wake notice closing a [`parked`] span — how the wait ended and what
/// it cost in engine-side probes.
pub fn woken(reason: &str, polls_used: u64) -> EventLine {
    EventLine {
        glyph: "▶",
        tone: Tone::Info,
        strong: true,
        body: format!(
            "wait ended after {polls_used} probe{}:",
            if polls_used == 1 { "" } else { "s" }
        ),
        detail: Some(
            match reason {
                "changed" => "the watched state changed.",
                "deadline_expired" => "the deadline expired with no change.",
                other => other,
            }
            .to_string(),
        ),
    }
}

/// The step-boundary injection notice — the plain surface's record that a
/// queued prompt was steered into the running turn.
pub fn steered(text: &str) -> EventLine {
    EventLine {
        glyph: "↪",
        tone: Tone::Info,
        strong: true,
        body: "steered into the running turn:".to_string(),
        detail: Some(text.to_string()),
    }
}

pub fn compaction(
    before_tokens: u64,
    after_tokens: u64,
    evicted: usize,
    deduped: usize,
    superseded: usize,
    aged: usize,
    summarized: usize,
) -> EventLine {
    // Name only the mechanisms that actually fired — most passes use one
    // or two, and a line of zeros reads as noise.
    let mut parts: Vec<String> = Vec::new();
    for (count, label) in [
        (evicted, "evicted"),
        (deduped, "deduped"),
        (superseded, "superseded"),
        (aged, "aged"),
        (summarized, "summarized"),
    ] {
        if count > 0 {
            parts.push(format!("{count} {label}"));
        }
    }
    EventLine {
        glyph: "⤵",
        tone: Tone::Info,
        strong: false,
        body: format!(
            "compacted context: {before_tokens} → {after_tokens} tokens ({})",
            parts.join(", ")
        ),
        detail: None,
    }
}

/// An event this build cannot decode, emitted by a newer stella.
///
/// Rendered rather than dropped: the realistic way the TUI meets one of these
/// is replaying a session journal written by a newer binary (`stella resume`),
/// and silently omitting events would leave unexplained holes in a transcript
/// the user is reading as a record. Muted, one line, tag only — enough to show
/// that something happened and that this build is the reason it is not
/// legible, without pretending to know what it was.
pub fn unknown_event(event_type: &str) -> EventLine {
    EventLine {
        glyph: "?",
        tone: Tone::Muted,
        strong: false,
        body: format!("unrecognized event `{event_type}`"),
        detail: Some("emitted by a newer stella".to_string()),
    }
}

/// The spend line. Visibility policy stays surface-side (the plain surface
/// suppresses ticks in `BudgetMode::Off`; the deck shows every tick and may
/// append the mode as detail).
pub fn budget_tick(spent_usd: f64, limit_usd: Option<f64>) -> EventLine {
    EventLine {
        glyph: "$",
        tone: Tone::Muted,
        strong: false,
        body: format!("spend: {}", spend_amount(spent_usd, limit_usd)),
        detail: None,
    }
}

pub fn provider_fallback(from: &str, to: &str, reason: &str) -> EventLine {
    EventLine {
        glyph: "⚠",
        tone: Tone::Warn,
        strong: true,
        body: format!("provider fallback {from} → {to}:"),
        detail: Some(reason.to_string()),
    }
}

#[allow(clippy::too_many_arguments)] // mirrors the event's metering fields 1:1
pub fn step_usage(
    step: usize,
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    cost_usd: f64,
    duration_ms: u64,
    retries: u32,
    tool_calls: usize,
) -> EventLine {
    let cached = if cached_input_tokens > 0 {
        format!(" ({} cached)", fmt_tokens(cached_input_tokens))
    } else {
        String::new()
    };
    let retried = if retries > 0 {
        format!(" · {retries} retry")
    } else {
        String::new()
    };
    let tools = if tool_calls > 0 {
        format!(
            " · {tool_calls} tool call{}",
            if tool_calls == 1 { "" } else { "s" }
        )
    } else {
        String::new()
    };
    EventLine {
        glyph: "·",
        tone: Tone::Muted,
        strong: false,
        body: format!(
            "step {} · {model} · {}{cached} in → {} out · {} · {:.1}s{retried}{tools}",
            step + 1,
            fmt_tokens(input_tokens),
            fmt_tokens(output_tokens),
            fmt_cost(cost_usd),
            duration_ms as f64 / 1000.0,
        ),
        detail: None,
    }
}

pub fn goal_verdict(round: usize, met: bool, reasoning: &str) -> EventLine {
    if met {
        EventLine {
            glyph: "✓",
            tone: Tone::Success,
            strong: true,
            body: format!("verifier verdict (round {round}): goal met — {reasoning}"),
            detail: None,
        }
    } else {
        EventLine {
            glyph: "○",
            tone: Tone::Warn,
            strong: false,
            body: format!("verifier verdict (round {round}): not yet met — {reasoning}"),
            detail: None,
        }
    }
}

/// A sub-agent's lifecycle bracket (#922) — the only place a reader sees
/// that a bounded child turn ran at all, since the child's own narration is
/// dropped at the parent boundary by design.
///
/// The finish line leads with what the child *saved*: the messages it
/// absorbed are context the parent never has to re-send, and that number is
/// the whole reason the primitive exists. Cost sits beside it so the trade
/// reads at a glance.
pub fn sub_agent(phase: &stella_protocol::SubAgentPhase) -> EventLine {
    use stella_protocol::{SubAgentPhase, SubAgentStatus};
    match phase {
        SubAgentPhase::Started {
            agent_id,
            instruction_preview,
            write_access,
            ..
        } => EventLine {
            glyph: "⤷",
            tone: Tone::Muted,
            strong: false,
            body: format!(
                "sub-agent {agent_id} ({}): {instruction_preview}",
                if *write_access { "write" } else { "read-only" }
            ),
            detail: None,
        },
        SubAgentPhase::Finished {
            agent_id,
            status,
            cost_usd,
            steps,
            absorbed_messages,
            reason,
            ..
        } => {
            let (glyph, tone) = match status {
                SubAgentStatus::Completed => ("✓", Tone::Success),
                SubAgentStatus::Incomplete => ("○", Tone::Warn),
                SubAgentStatus::Refused => ("✗", Tone::Error),
            };
            EventLine {
                glyph,
                tone,
                strong: false,
                body: match reason {
                    Some(reason) => format!("sub-agent {agent_id}: {reason}"),
                    None => format!("sub-agent {agent_id} done"),
                },
                detail: Some(format!(
                    "· {steps} step{} · {absorbed_messages} msgs absorbed · {}",
                    if *steps == 1 { "" } else { "s" },
                    fmt_cost(*cost_usd)
                )),
            }
        }
    }
}

pub fn file_change(path: &str, kind: FileChangeKind) -> EventLine {
    EventLine {
        glyph: "±",
        tone: Tone::Info,
        strong: false,
        body: format!("{} {path}", file_change_verb(kind)),
        detail: None,
    }
}

/// One recall, on a surface that gets exactly one line for it.
///
/// The deck renders a recall as a table (`render::entry`); this surface prints
/// one line per event and cannot fold, so it states the same *facts* in the
/// order they answer questions: how much did the model get, what did it cost,
/// was recall the reason the turn felt slow, what kinds came back, and from
/// which legs.
///
/// The two surfaces used to disagree about what a recall even is — this one
/// named the provider mix and no labels, the deck named the labels and no
/// provider mix, and neither said the latency the wire had carried since #875.
/// `kinds` and `cited` are both passed in so the wording stays here, in the one
/// module that owns wording.
///
/// `latency_ms` of `0` means *not measured* on the wire, so it is omitted
/// rather than printed as `0ms`.
pub fn context_recall(
    frames: usize,
    tokens: u32,
    latency_ms: u32,
    kinds: &str,
    cited: &str,
) -> EventLine {
    let mut body = format!("recalled {frames} frames · {tokens} tok");
    if latency_ms > 0 {
        body.push_str(&format!(" · {latency_ms}ms"));
    }
    if !kinds.is_empty() {
        body.push_str(&format!(" · {kinds}"));
    }
    EventLine {
        glyph: "◈",
        tone: Tone::Info,
        strong: false,
        body,
        detail: (!cited.is_empty()).then(|| format!("via {cited}")),
    }
}

/// `4 symbol, 1 episode` — the recall's kind histogram, in the frames' own
/// order of first appearance.
///
/// Kind is the field that changes how a recall row is *read*: four graph
/// symbols and one episodic memory cost the prompt the same tokens and say
/// entirely different things about what retrieval did. Ordering by first
/// appearance rather than by count keeps the string stable across turns that
/// recall the same mix in a different quantity.
#[must_use]
pub fn frame_kind_label(frames: &[stella_protocol::ContextFrameRef]) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for frame in frames {
        let kind = if frame.kind.is_empty() {
            "frame"
        } else {
            frame.kind.as_str()
        };
        match counts.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, n)) => *n += 1,
            None => counts.push((kind, 1)),
        }
    }
    counts
        .into_iter()
        .map(|(kind, n)| format!("{n} {kind}"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn context_write(provider: &str, upserts: u32, superseded: u32) -> EventLine {
    EventLine {
        glyph: "◈",
        tone: Tone::Muted,
        strong: false,
        body: format!(
            "context write-back via {provider}: {upserts} upserts, {superseded} superseded"
        ),
        detail: None,
    }
}

pub fn media_progress(kind: MediaKind, artifact_id: &str, state: &MediaJobState) -> EventLine {
    match state {
        MediaJobState::Failed { reason } => EventLine {
            glyph: "✗",
            tone: Tone::Error,
            strong: false,
            body: format!("{kind:?} job {artifact_id} failed: {reason}"),
            detail: None,
        },
        other => EventLine {
            glyph: "▣",
            tone: Tone::Info,
            strong: false,
            body: format!("{kind:?} job {artifact_id}: {}", media_state_label(other)),
            detail: None,
        },
    }
}

pub fn media_complete(label: &str, path: &str, kind: MediaKind) -> EventLine {
    EventLine {
        glyph: "▣",
        tone: Tone::Success,
        strong: false,
        body: format!("{label} ready: {path} ({})", media_kind_label(kind)),
        detail: None,
    }
}

pub fn verdict(passed: bool, deterministic: bool, summary: &str) -> EventLine {
    let source = if deterministic {
        "deterministic"
    } else {
        "model verifier"
    };
    EventLine {
        glyph: if passed { "✓" } else { "✗" },
        tone: if passed { Tone::Success } else { Tone::Error },
        strong: false,
        body: format!("verify ({source}):"),
        detail: Some(summary.to_string()),
    }
}

pub fn scope_review(
    summary: &str,
    steps: usize,
    estimated_files: u32,
    estimated_cost_usd: Option<f64>,
) -> EventLine {
    let cost = estimated_cost_usd
        .map(|c| format!(", ~${c:.2}"))
        .unwrap_or_default();
    EventLine {
        glyph: "⌾",
        tone: Tone::Warn,
        strong: true,
        body: format!("scope review: {summary} ({steps} steps, ~{estimated_files} files{cost})"),
        detail: None,
    }
}

/// How many distinct files a hunk list touches.
pub fn distinct_paths(hunks: &[stella_protocol::ProposedHunk]) -> usize {
    let mut paths: Vec<&str> = hunks.iter().map(|h| h.path.as_str()).collect();
    paths.sort_unstable();
    paths.dedup();
    paths.len()
}

/// The per-hunk approval gate's headline. The hunks themselves render through
/// each surface's own diff machinery — this is the one line that says a write
/// is parked waiting on a person.
pub fn hunk_review(tool: &str, hunks: usize, files: usize) -> EventLine {
    EventLine {
        glyph: "⌾",
        tone: Tone::Warn,
        strong: true,
        body: format!(
            "hunk review: {hunks} hunk{} across {files} file{} from {tool}",
            if hunks == 1 { "" } else { "s" },
            if files == 1 { "" } else { "s" },
        ),
        detail: None,
    }
}

/// The question line only. The structured options — and the binding
/// free-text affordance — are presented by each surface's own interaction
/// machinery (numbered stdin list vs the deck's answer card).
pub fn ask_user(question: &str) -> EventLine {
    EventLine {
        glyph: "?",
        tone: Tone::Warn,
        strong: true,
        body: question.to_string(),
        detail: None,
    }
}

pub fn commit(sha: &str, message: &str) -> EventLine {
    // `get(..8)` not a slice: a sha shorter than 8 bytes (or a non-ASCII
    // test fixture) must fall back whole rather than panic.
    let short = sha.get(..8).unwrap_or(sha);
    EventLine {
        glyph: "●",
        tone: Tone::Success,
        strong: false,
        body: format!("committed {short}"),
        detail: Some(message.to_string()),
    }
}

pub fn pr(url: &str, status: PrStatus, number: Option<u64>, ci: Option<CiStatus>) -> EventLine {
    let ident = match number {
        Some(n) => format!("PR #{n}"),
        None => "PR".to_string(),
    };
    let ci_suffix = match ci {
        Some(ci) => format!(" · ci {}", ci_status_label(ci)),
        None => String::new(),
    };
    EventLine {
        glyph: "⇡",
        tone: Tone::Info,
        strong: false,
        body: format!("{ident} {}{ci_suffix}: {url}", pr_status_label(status)),
        detail: None,
    }
}

/// One-line task-board digest: progress counts plus the subject the agent
/// is currently on (the full checklist is a deck surface; the plain REPL
/// gets the digest).
pub fn task_board(tasks: &[TaskItem]) -> EventLine {
    let done = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Completed)
        .count();
    let total = tasks.len();
    let active = tasks
        .iter()
        .find(|t| t.status == TaskStatus::InProgress)
        .map(|t| t.subject.clone());
    EventLine {
        glyph: "☰",
        tone: Tone::Info,
        strong: false,
        body: format!("tasks {done}/{total}"),
        detail: active.map(|subject| format!("· {subject}")),
    }
}

/// Routing (stdout vs stderr, transcript row vs toast) stays surface-side.
pub fn error(message: &str, retryable: bool) -> EventLine {
    let label = if retryable { "warning" } else { "error" };
    EventLine {
        glyph: "✗",
        tone: Tone::Error,
        strong: false,
        body: format!("{label}: {message}"),
        detail: None,
    }
}

pub fn complete(model: &str, cost_usd: f64) -> EventLine {
    EventLine {
        glyph: "✓",
        tone: Tone::Success,
        strong: true,
        body: format!("complete · {model} · {}", fmt_cost(cost_usd)),
        detail: None,
    }
}

// ── Event dispatcher ─────────────────────────────────────────────────────────

/// The per-variant lookup table over a raw event stream. `None` for the
/// variants whose presentation is structural per surface (streamed
/// `Text`/`Reasoning`, `Stage`, and the tool cards — see the module doc); a
/// consumer must handle those (or deliberately skip them) itself, and gets
/// every annotation variant — including future ones — from this one table.
pub fn event_line(event: &AgentEvent) -> Option<EventLine> {
    match event {
        AgentEvent::Stage { .. }
        | AgentEvent::Text { .. }
        | AgentEvent::TextDelta { .. }
        | AgentEvent::Reasoning { .. }
        | AgentEvent::ToolStart { .. }
        | AgentEvent::ToolResult { .. }
        | AgentEvent::UsageIncomplete { .. }
        // Context receipts (spec §4/§5) are observability, not transcript
        // narration — they never produce a rendered line.
        | AgentEvent::BlockRegistered { .. }
        | AgentEvent::StepManifest { .. }
        // A discarded speculation is internal accounting for read-only work
        // that never reached the transcript — observability, not narration.
        | AgentEvent::SpeculationDiscarded { .. }
        // The delivery decision (#2942) is the parseable record of something
        // the pipeline ALREADY narrates in prose: an unproven delivery warns on
        // the rail, a withheld one names where the work was kept, and the files
        // themselves arrive as `FileChange` lines. A line here would be the
        // third telling of one fact — which is why the event is declared
        // `Surfaced` on the observatory's journal, not on this surface.
        | AgentEvent::CandidateDelivery { .. }
        // Proof steps are the rail's data ([`crate::proof`]), not narration.
        // They are a state machine whose CURRENT value is the whole point, so
        // a surface renders the folded rail; replaying each transition as a
        // scrollback line would bury the answer under its own history.
        | AgentEvent::Proof { .. }
        // Typed decision events (receipts spec §6.3/§6.4) are the parseable
        // twins of prose the stream already narrates (`Steered`/`Error`
        // carry the loop/budget/retry story; policy denials surface as tool
        // errors) — rendering them too would say everything twice.
        | AgentEvent::LoopDetected { .. }
        | AgentEvent::BudgetDenied { .. }
        | AgentEvent::RetriesExhausted { .. }
        | AgentEvent::PolicyDecision { .. } => None,
        AgentEvent::Unknown { event_type, .. } => Some(unknown_event(event_type)),
        AgentEvent::SubAgent { phase } => Some(sub_agent(phase)),
        AgentEvent::Retry { attempt, reason } => Some(retry(*attempt, reason)),
        AgentEvent::Steered { text, .. } => Some(steered(text)),
        AgentEvent::TurnParked {
            description,
            poll_interval_secs,
            deadline_secs,
        } => Some(parked(description, *poll_interval_secs, *deadline_secs)),
        AgentEvent::TurnWoken { reason, polls_used } => Some(woken(reason, *polls_used)),
        AgentEvent::Compaction {
            before_tokens,
            after_tokens,
            evicted,
            deduped,
            superseded,
            aged,
            summarized,
            // Block identities + effective budget ride the event for receipts
            // (spec §6.2); the transcript line stays a count summary.
            ..
        } => Some(compaction(
            *before_tokens,
            *after_tokens,
            *evicted,
            *deduped,
            *superseded,
            *aged,
            *summarized,
        )),
        AgentEvent::BudgetTick {
            spent_usd,
            limit_usd,
            ..
        } => Some(budget_tick(*spent_usd, *limit_usd)),
        AgentEvent::ProviderFallback { from, to, reason } => {
            Some(provider_fallback(from, to, reason))
        }
        AgentEvent::StepUsage {
            step,
            model,
            input_tokens,
            output_tokens,
            cached_input_tokens,
            cost_usd,
            duration_ms,
            retries,
            tool_calls,
            // Estimator calibration feedback, not display material.
            ..
        } => Some(step_usage(
            *step,
            model,
            *input_tokens,
            *output_tokens,
            *cached_input_tokens,
            *cost_usd,
            *duration_ms,
            *retries,
            *tool_calls,
        )),
        AgentEvent::GoalVerdict {
            round,
            met,
            reasoning,
            ..
        } => Some(goal_verdict(*round, *met, reasoning)),
        AgentEvent::FileChange { path, kind, .. } => Some(file_change(path, *kind)),
        AgentEvent::ContextRecall {
            frames,
            provider_mix,
            tokens,
            latency_ms,
            ..
        } => Some(context_recall(
            frames.len(),
            *tokens,
            *latency_ms,
            &frame_kind_label(frames),
            &provider_mix_label(provider_mix),
        )),
        AgentEvent::ContextWrite {
            provider,
            upserts,
            superseded,
        } => Some(context_write(provider, *upserts, *superseded)),
        AgentEvent::MediaProgress {
            artifact_id,
            kind,
            state,
        } => Some(media_progress(*kind, artifact_id, state)),
        AgentEvent::MediaComplete { artifact } => Some(media_complete(
            &artifact.label,
            &artifact.path,
            artifact.kind,
        )),
        AgentEvent::Verdict { passed, evidence } => Some(verdict(
            *passed,
            evidence.deterministic,
            &evidence.summary,
        )),
        AgentEvent::ScopeReview { proposal } => Some(scope_review(
            &proposal.summary,
            proposal.steps.len(),
            proposal.estimated_files,
            proposal.estimated_cost_usd,
        )),
        AgentEvent::HunkReview { proposal } => Some(hunk_review(
            &proposal.tool,
            proposal.hunks.len(),
            distinct_paths(&proposal.hunks),
        )),
        AgentEvent::AskUser { question, .. } => Some(ask_user(question)),
        AgentEvent::Commit { sha, message } => Some(commit(sha, message)),
        AgentEvent::Pr {
            url,
            status,
            number,
            ci,
        } => Some(pr(url, *status, *number, *ci)),
        AgentEvent::TaskUpdate { tasks } => Some(task_board(tasks)),
        AgentEvent::Error { message, retryable } => Some(error(message, *retryable)),
        AgentEvent::TurnComplete { model, cost_usd } => Some(complete(model, *cost_usd)),
        AgentEvent::RunComplete { model, cost_usd } => Some(complete(model, *cost_usd)),
    }
}

// ── Enum label tables ────────────────────────────────────────────────────────

/// The word the deck says out loud for a stage.
///
/// For one of the host's own boundaries this is the deck's phrasing, which is
/// not always the wire spelling — `context_recall` reads "context recall",
/// because the underscore is a wire detail and the deck writes prose.
///
/// For a **contributed** stage it is the plugin's own word, verbatim. That is
/// the honest fallback and the same one the `/models` role table settled on
/// (`envelope::roles`): the deck has no word for a stage it has never heard of,
/// and inventing one — "plugin", "custom", "other" — would name the row after a
/// category instead of after itself.
pub fn stage_label(stage: &StageName) -> &str {
    let Some(kind) = stage.kind() else {
        return stage.as_str();
    };
    match kind {
        StageKind::Triage => "triage",
        StageKind::ContextRecall => "context recall",
        StageKind::Research => "research",
        StageKind::Plan => "plan",
        StageKind::ScopeReview => "scope review",
        StageKind::Witness => "witness",
        StageKind::Execute => "execute",
        StageKind::Verify => "verify",
        StageKind::Verdict => "verdict",
        StageKind::Reflect => "reflect",
        StageKind::ContextWrite => "context write",
        StageKind::Complete => "complete",
    }
}

pub fn budget_mode_label(mode: BudgetMode) -> &'static str {
    match mode {
        BudgetMode::Off => "off",
        BudgetMode::Observed => "observed",
        BudgetMode::Enforced => "enforced",
    }
}

pub fn media_kind_label(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Image => "image",
        MediaKind::Svg => "svg",
        MediaKind::Video => "video",
    }
}

pub fn pr_status_label(status: PrStatus) -> &'static str {
    match status {
        PrStatus::Draft => "draft",
        PrStatus::Open => "open",
        PrStatus::Merged => "merged",
        PrStatus::Closed => "closed",
    }
}

/// A flat display label for a PR's aggregate CI verdict.
pub fn ci_status_label(status: CiStatus) -> &'static str {
    match status {
        CiStatus::Pending => "pending",
        CiStatus::Running => "running",
        CiStatus::Passing => "passing",
        CiStatus::Failing => "failing",
    }
}

/// A flat display label for a media job state (the wire enum is tagged).
pub fn media_state_label(state: &MediaJobState) -> String {
    match state {
        MediaJobState::Queued => "queued".to_string(),
        MediaJobState::Running => "running".to_string(),
        MediaJobState::Succeeded => "succeeded".to_string(),
        MediaJobState::Failed { reason } => format!("failed: {reason}"),
    }
}

/// Past-tense verb for a `FileChange` transcript line.
pub fn file_change_verb(kind: FileChangeKind) -> &'static str {
    match kind {
        FileChangeKind::Read => "read",
        FileChangeKind::Created => "created",
        FileChangeKind::Modified => "modified",
        FileChangeKind::Deleted => "deleted",
    }
}

/// The CRUD badge letter for a file-change kind — the vocabulary the
/// files-touched panels share with the plain CLI's registry ledger.
pub fn crud_letter(kind: FileChangeKind) -> &'static str {
    match kind {
        FileChangeKind::Read => "R",
        FileChangeKind::Created => "C",
        FileChangeKind::Modified => "U",
        FileChangeKind::Deleted => "D",
    }
}

// ── Number / amount formatting ───────────────────────────────────────────────

/// Render a token count compactly: `842`, `12.3k`, `1.2M`.
pub fn fmt_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// A USD cost at the 4-decimal precision every spend line uses.
pub fn fmt_cost(cost_usd: f64) -> String {
    format!("${cost_usd:.4}")
}

/// Spend against an optional limit — the HUD's spend gauge and the budget
/// tick line share this exact form.
pub fn spend_amount(spent_usd: f64, limit_usd: Option<f64>) -> String {
    match limit_usd {
        Some(limit) => format!("${spent_usd:.4} / ${limit:.2}"),
        None => format!("${spent_usd:.4}"),
    }
}

/// `2×code-graph, 1×memory` — a recall's provider mix as cited text.
pub fn provider_mix_label(mix: &[ProviderShare]) -> String {
    mix.iter()
        .map(|share| format!("{}×{}", share.frames, share.provider))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::{ContextFrameRef, MediaArtifactRef, ScopeProposal, VerdictEvidence};

    // ── Byte-exact fixtures ──────────────────────────────────────────────
    //
    // These strings are the plain CLI's pre-extraction output (issue #66):
    // `stella-cli` prints `"  {}"` around `EventLine::text()`, so each
    // fixture pins the visible line the extraction must not change.

    #[test]
    fn wording_matches_the_pre_extraction_plain_renderer_byte_for_byte() {
        assert_eq!(retry(2, "rate limited").text(), "↻ retry #2: rate limited");
        assert_eq!(
            compaction(10_000, 4_000, 3, 2, 0, 0, 0).text(),
            "⤵ compacted context: 10000 → 4000 tokens (3 evicted, 2 deduped)"
        );
        assert_eq!(
            budget_tick(0.42, Some(2.5)).text(),
            "$ spend: $0.4200 / $2.50"
        );
        assert_eq!(budget_tick(0.42, None).text(), "$ spend: $0.4200");
        assert_eq!(
            provider_fallback("zai", "anthropic", "circuit open").text(),
            "⚠ provider fallback zai → anthropic: circuit open"
        );
        assert_eq!(
            step_usage(3, "glm-5.2", 12_000, 450, 9_000, 0.0042, 1_830, 1, 4).text(),
            "· step 4 · glm-5.2 · 12.0k (9.0k cached) in → 450 out · $0.0042 · 1.8s · 1 retry · 4 tool calls"
        );
        assert_eq!(
            step_usage(0, "glm-5.2", 842, 10, 0, 0.001, 500, 0, 1).text(),
            "· step 1 · glm-5.2 · 842 in → 10 out · $0.0010 · 0.5s · 1 tool call"
        );
        assert_eq!(
            goal_verdict(2, true, "tests pass").text(),
            "✓ verifier verdict (round 2): goal met — tests pass"
        );
        assert_eq!(
            goal_verdict(1, false, "still failing").text(),
            "○ verifier verdict (round 1): not yet met — still failing"
        );
        assert_eq!(
            file_change("src/lib.rs", FileChangeKind::Modified).text(),
            "± modified src/lib.rs"
        );
        assert_eq!(
            context_recall(2, 120, 34, "2 symbol", "2×code-graph").text(),
            "◈ recalled 2 frames · 120 tok · 34ms · 2 symbol via 2×code-graph"
        );
        // `latency_ms: 0` means *not measured* on the wire, never "instant" —
        // so it is omitted rather than rendered as a `0ms` nobody measured.
        assert_eq!(
            context_recall(2, 120, 0, "2 symbol", "2×code-graph").text(),
            "◈ recalled 2 frames · 120 tok · 2 symbol via 2×code-graph"
        );
        assert_eq!(
            context_write("mem0", 3, 1).text(),
            "◈ context write-back via mem0: 3 upserts, 1 superseded"
        );
        assert_eq!(
            media_progress(MediaKind::Image, "a1", &MediaJobState::Running).text(),
            "▣ Image job a1: running"
        );
        assert_eq!(
            media_progress(
                MediaKind::Video,
                "a2",
                &MediaJobState::Failed {
                    reason: "nsfw".into()
                }
            )
            .text(),
            "✗ Video job a2 failed: nsfw"
        );
        assert_eq!(
            media_complete("diagram", ".stella/artifacts/a2.png", MediaKind::Image).text(),
            "▣ diagram ready: .stella/artifacts/a2.png (image)"
        );
        assert_eq!(
            verdict(true, true, "flip oracle passed").text(),
            "✓ verify (deterministic): flip oracle passed"
        );
        assert_eq!(
            verdict(false, false, "inconclusive").text(),
            "✗ verify (model verifier): inconclusive"
        );
        assert_eq!(
            scope_review("refactor auth", 2, 12, Some(1.25)).text(),
            "⌾ scope review: refactor auth (2 steps, ~12 files, ~$1.25)"
        );
        assert_eq!(
            scope_review("small fix", 1, 1, None).text(),
            "⌾ scope review: small fix (1 steps, ~1 files)"
        );
        assert_eq!(ask_user("which database?").text(), "? which database?");
        assert_eq!(
            commit("abc1234567", "feat: x").text(),
            "● committed abc12345 feat: x"
        );
        assert_eq!(
            commit("abc", "short sha").text(),
            "● committed abc short sha"
        );
        assert_eq!(
            pr("https://x/pr/1", PrStatus::Open, None, None).text(),
            "⇡ PR open: https://x/pr/1"
        );
        assert_eq!(
            pr(
                "https://x/pr/183",
                PrStatus::Open,
                Some(183),
                Some(CiStatus::Failing)
            )
            .text(),
            "⇡ PR #183 open · ci failing: https://x/pr/183"
        );
        assert_eq!(error("boom", false).text(), "✗ error: boom");
        assert_eq!(error("blip", true).text(), "✗ warning: blip");
        assert_eq!(
            complete("glm-5.2", 0.0123).text(),
            "✓ complete · glm-5.2 · $0.0123"
        );
    }

    #[test]
    fn fmt_helpers_keep_their_exact_forms() {
        assert_eq!(fmt_tokens(842), "842");
        assert_eq!(fmt_tokens(12_300), "12.3k");
        assert_eq!(fmt_tokens(1_200_000), "1.2M");
        assert_eq!(fmt_cost(0.0042), "$0.0042");
        assert_eq!(spend_amount(0.42, Some(2.0)), "$0.4200 / $2.00");
        assert_eq!(spend_amount(0.42, None), "$0.4200");
        assert_eq!(
            provider_mix_label(&[
                ProviderShare {
                    provider: "code-graph".into(),
                    frames: 2,
                },
                ProviderShare {
                    provider: "memory".into(),
                    frames: 1,
                },
            ]),
            "2×code-graph, 1×memory"
        );
        assert_eq!(provider_mix_label(&[]), "");
    }

    #[test]
    fn label_tables_cover_every_variant() {
        assert_eq!(
            stage_label(&StageKind::ContextRecall.into()),
            "context recall"
        );
        assert_eq!(budget_mode_label(BudgetMode::Enforced), "enforced");
        assert_eq!(media_kind_label(MediaKind::Svg), "svg");
        assert_eq!(pr_status_label(PrStatus::Merged), "merged");
        assert_eq!(
            media_state_label(&MediaJobState::Failed { reason: "x".into() }),
            "failed: x"
        );
        assert_eq!(file_change_verb(FileChangeKind::Created), "created");
        assert_eq!(crud_letter(FileChangeKind::Deleted), "D");
    }

    /// **The witness for a contributed stage's word.** The deck says the
    /// plugin's own name back, verbatim — not a category like "plugin", and
    /// not the nearest host stage it happens to resemble.
    ///
    /// `triage-lite` is the load-bearing case: it *contains* a host stage's
    /// name, so anything that resolved by prefix or substring would silently
    /// relabel it `triage` and claim the turn ran a stage it never ran.
    #[test]
    fn a_contributed_stage_is_labelled_with_its_own_word() {
        for word in ["triage-lite", "vera/witness", "sast", "spec-check"] {
            let stage = StageName::new(word);
            assert!(stage.kind().is_none(), "{word} must be contributed");
            assert_eq!(stage_label(&stage), word);
        }
    }

    /// The stage after `verify` is `verdict` — never `verifier`, which is the
    /// *model* that produces it. Naming the stage after its model recreates the
    /// `verify → verifier` adjacency #1394's rename existed to remove, and it
    /// put the HUD one word away from the statline, which has always said
    /// `VERDICT` (#1465).
    #[test]
    fn the_verdict_stage_is_never_labelled_after_the_model_that_runs_it() {
        assert_eq!(stage_label(&StageKind::Verify.into()), "verify");
        assert_eq!(stage_label(&StageKind::Verdict.into()), "verdict");
    }

    // ── Dispatch coverage ────────────────────────────────────────────────

    #[test]
    fn event_line_maps_every_annotation_variant_and_skips_the_structural_ones() {
        let annotations: Vec<AgentEvent> = vec![
            AgentEvent::Retry {
                attempt: 1,
                reason: "x".into(),
            },
            AgentEvent::TurnParked {
                description: "CI settles".into(),
                poll_interval_secs: 5,
                deadline_secs: 600,
            },
            AgentEvent::TurnWoken {
                reason: "changed".into(),
                polls_used: 3,
            },
            AgentEvent::Compaction {
                before_tokens: 2,
                after_tokens: 1,
                evicted: 1,
                deduped: 0,
                superseded: 0,
                aged: 0,
                summarized: 0,
                evicted_blocks: vec![],
                deduped_blocks: vec![],
                superseded_blocks: vec![],
                aged_blocks: vec![],
                summarized_blocks: vec![],
                rewrites: vec![],
                effective_budget_tokens: 0,
                calibration_factor: 0.0,
            },
            AgentEvent::BudgetTick {
                spent_usd: 0.1,
                limit_usd: None,
                mode: BudgetMode::Observed,
                session_spent_usd: None,
                session_limit_usd: None,
                deadline_remaining_ms: None,
            },
            AgentEvent::ProviderFallback {
                from: "a".into(),
                to: "b".into(),
                reason: "x".into(),
            },
            AgentEvent::StepUsage {
                upstream_provider: None,
                output_text: None,
                step: 0,
                role: stella_protocol::ModelCallRole::Worker,
                provider: "test".into(),
                model: "m".into(),
                input_tokens: 1,
                output_tokens: 1,
                cached_input_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: None,
                estimated_input_tokens: 0,
                cost_usd: 0.0,
                duration_ms: 1,
                retries: 0,
                tool_calls: 0,
                complete: true,
                finish_reason: None,
            },
            AgentEvent::GoalVerdict {
                round: 1,
                met: true,
                reasoning: "x".into(),
                cost_usd: 0.0,
            },
            AgentEvent::FileChange {
                path: "a.rs".into(),
                kind: FileChangeKind::Created,
                added: 0,
                removed: 0,
                diff: None,
            },
            AgentEvent::ContextRecall {
                frames: vec![ContextFrameRef {
                    id: None,
                    citation_label: "l".into(),
                    provider: "p".into(),
                    source: "s".into(),
                    kind: String::new(),
                    uri: None,
                    method: None,
                    token_cost: 1,
                    block_id: None,
                    content_digest: None,
                }],
                provider_mix: vec![],
                tokens: 1,
                usage: None,
                latency_ms: 0,
                used_ann_index: None,
            },
            AgentEvent::ContextWrite {
                provider: "p".into(),
                upserts: 1,
                superseded: 0,
            },
            AgentEvent::MediaProgress {
                artifact_id: "a".into(),
                kind: MediaKind::Image,
                state: MediaJobState::Queued,
            },
            AgentEvent::MediaComplete {
                artifact: MediaArtifactRef {
                    id: "a".into(),
                    kind: MediaKind::Image,
                    path: "p".into(),
                    label: "l".into(),
                },
            },
            AgentEvent::Verdict {
                passed: true,
                evidence: VerdictEvidence {
                    summary: "s".into(),
                    deterministic: true,
                    evidence_refs: vec![],
                    ladder: None,
                },
            },
            AgentEvent::ScopeReview {
                proposal: ScopeProposal {
                    summary: "s".into(),
                    steps: vec![],
                    estimated_files: 1,
                    estimated_cost_usd: None,
                    ..Default::default()
                },
            },
            AgentEvent::AskUser {
                id: "q".into(),
                question: "?".into(),
                options: vec![],
            },
            AgentEvent::Commit {
                sha: "abc".into(),
                message: "m".into(),
            },
            AgentEvent::Pr {
                url: "u".into(),
                status: PrStatus::Open,
                number: Some(1),
                ci: Some(CiStatus::Passing),
            },
            AgentEvent::Error {
                message: "e".into(),
                retryable: false,
            },
            AgentEvent::RunComplete {
                model: "m".into(),
                cost_usd: 0.0,
            },
            // Both phases of a sub-agent bracket (#922): the child's own
            // narration is filtered out at the parent boundary, so if these
            // two rows do not render, a child turn is completely invisible.
            AgentEvent::SubAgent {
                phase: stella_protocol::SubAgentPhase::Started {
                    agent_id: "search-1".into(),
                    instruction_preview: "find it".into(),
                    budget_usd: Some(0.1),
                    write_access: false,
                    depth: 1,
                },
            },
            AgentEvent::SubAgent {
                phase: stella_protocol::SubAgentPhase::Finished {
                    agent_id: "search-1".into(),
                    status: stella_protocol::SubAgentStatus::Completed,
                    summary: "it is in retry.rs".into(),
                    truncated: false,
                    cost_usd: 0.004,
                    steps: 5,
                    absorbed_messages: 9,
                    reason: None,
                },
            },
        ];
        for event in &annotations {
            assert!(
                event_line(event).is_some(),
                "annotation variant unmapped: {event:?}"
            );
        }

        use stella_protocol::{ToolCall, ToolOutput};
        let structural: Vec<AgentEvent> = vec![
            AgentEvent::Stage {
                name: StageKind::Execute.into(),
                scope: stella_protocol::StageScope::Run,
            },
            AgentEvent::Text { text: "t".into() },
            AgentEvent::Reasoning { delta: "r".into() },
            AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: "c".into(),
                    name: "n".into(),
                    input: serde_json::Value::Null,
                },
            },
            AgentEvent::ToolResult {
                call_id: "c".into(),
                output: ToolOutput::Ok {
                    content: "o".into(),
                    data: None,
                },
                duration_ms: 1,
                speculated: false,
            },
        ];
        for event in &structural {
            assert!(
                event_line(event).is_none(),
                "structural variant must stay surface-owned: {event:?}"
            );
        }
    }

    fn recall_frame(label: &str, provider: &str, kind: &str, tokens: u32) -> ContextFrameRef {
        ContextFrameRef {
            id: None,
            citation_label: label.into(),
            provider: provider.into(),
            source: provider.into(),
            kind: kind.into(),
            uri: None,
            method: None,
            token_cost: tokens,
            block_id: None,
            content_digest: None,
        }
    }

    /// The one-line surface names the kind mix, the cost, the latency, and the
    /// provider legs — all four, from the event alone.
    ///
    /// It used to name the provider mix and nothing else, while the deck named
    /// the citation labels and nothing else, so the two surfaces disagreed
    /// about what a recall *is* and neither reported the `latency_ms` the wire
    /// has carried since #875. The witness is that this string now answers
    /// "what came back, what did it cost, was it slow, and from where".
    #[test]
    fn event_line_recall_names_kinds_cost_latency_and_legs() {
        let line = event_line(&AgentEvent::ContextRecall {
            frames: vec![
                recall_frame("driver.rs", "code-graph", "symbol", 80),
                recall_frame("release 0.8.0", "workspace-memory", "episode", 40),
            ],
            provider_mix: vec![
                ProviderShare {
                    provider: "code-graph".into(),
                    frames: 1,
                },
                ProviderShare {
                    provider: "workspace-memory".into(),
                    frames: 1,
                },
            ],
            tokens: 120,
            usage: None,
            latency_ms: 34,
            used_ann_index: None,
        })
        .expect("recall is an annotation");
        assert_eq!(
            line.text(),
            "◈ recalled 2 frames · 120 tok · 34ms · 1 symbol, 1 episode \
             via 1×code-graph, 1×workspace-memory"
        );
    }

    /// A kind the emitter did not set renders as `frame`, not as a blank.
    ///
    /// Streams recorded before `ContextFrameRef::kind` existed carry an empty
    /// string, and an empty kind column reads as a rendering bug rather than
    /// as the missing field it is.
    #[test]
    fn frame_kind_label_names_an_absent_kind_rather_than_blanking_it() {
        assert_eq!(
            frame_kind_label(&[recall_frame("old", "code-graph", "", 10)]),
            "1 frame"
        );
    }
}
