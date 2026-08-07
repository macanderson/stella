//! Pure `AgentEvent` classifiers: the derived attributes the workspace fold
//! reads off an event without holding any state of its own — sparkline
//! intensity, implied lifecycle status, and the unified trace row.
//!
//! Split out of `deck.rs` when #2007's parked-wait clock pushed that file
//! against its 1500-line ceiling — the same `prompt_queue.rs` move, for the
//! same reason: relieve the ratchet by extracting a coherent cluster, never by
//! raising it. A pure move; the four functions are byte-identical to the ones
//! they replaced apart from the `pub(super)` a child module needs, and
//! [`super`] re-imports them so every call site is unchanged.

use stella_protocol::AgentEvent;

use super::TraceKind;
use crate::envelope::AgentStatus;

/// Activity intensity for the sparkline, by event kind. Edits and tool calls
/// read as "hot"; streaming text as "warm"; metering ticks as "cool".
pub(super) fn event_intensity(ev: &AgentEvent) -> u8 {
    match ev {
        AgentEvent::FileChange { .. } => 255,
        AgentEvent::ToolStart { .. } | AgentEvent::ToolResult { .. } => 210,
        AgentEvent::Stage { .. } => 170,
        AgentEvent::Text { .. } | AgentEvent::TextDelta { .. } => 130,
        AgentEvent::Reasoning { .. } => 90,
        AgentEvent::Commit { .. } | AgentEvent::Pr { .. } => 230,
        AgentEvent::BudgetTick { .. } | AgentEvent::StepUsage { .. } => 60,
        AgentEvent::Error { .. } => 255,
        // A proof step is a decision the run reached, not work it did. It is
        // real activity on the rail and none on the sparkline — pitched with
        // the stage boundaries it interleaves with, so a well-proven turn does
        // not read as busier than an unproven one doing the same edits.
        AgentEvent::Proof { .. } => 170,
        // A sub-agent bracket is a boundary, pitched with `Stage` for the
        // same reason: the child's real work already registers through its
        // forwarded tool calls and metering, so pricing the bracket as work
        // too would double-count one child as a burst of activity.
        AgentEvent::SubAgent { .. } => 170,
        // Explicit rather than falling through the wildcard: an undecodable
        // event is real activity, so it should register on the sparkline, but
        // this build cannot know whether it was hot (an edit) or cool (a
        // metering tick). Cool-but-present is the honest reading, and it keeps
        // a burst of future events from impersonating heavy edit activity.
        AgentEvent::Unknown { .. } => 60,
        // A park is the turn deliberately idling — the coolest honest signal,
        // pitched with the metering ticks so a long wait never reads as work.
        AgentEvent::TurnParked { .. } | AgentEvent::TurnWoken { .. } => 60,
        _ => 110,
    }
}

/// Lifecycle status implied by an event, or `None` if it doesn't move the
/// agent's lifecycle.
pub(super) fn status_from_event(ev: &AgentEvent) -> Option<AgentStatus> {
    match ev {
        AgentEvent::Complete { .. } => Some(AgentStatus::Done),
        AgentEvent::Error { retryable, .. } => Some(if *retryable {
            AgentStatus::Running
        } else {
            AgentStatus::Failed
        }),
        // Both user-response gates block the agent until answered — a scope
        // review is just as much "needs input" as an ask-user question.
        AgentEvent::AskUser { .. }
        | AgentEvent::ScopeReview { .. }
        | AgentEvent::HunkReview { .. } => Some(AgentStatus::WaitingInput),
        AgentEvent::Stage { .. }
        | AgentEvent::Text { .. }
        | AgentEvent::TextDelta { .. }
        | AgentEvent::Reasoning { .. }
        | AgentEvent::ToolStart { .. }
        | AgentEvent::ToolResult { .. }
        // A child turn is the parent working, so the lane stays Running
        // rather than falling through to "no lifecycle change". Explicit
        // because the wildcard below would otherwise let a long child run —
        // whose own narration is filtered out — read as an idle agent.
        | AgentEvent::SubAgent { .. }
        // A parked turn is the engine actively probing on its own clock —
        // alive, not waiting on the user — and the wake precedes the next
        // model call. Explicit for the same reason as `SubAgent`: a long
        // park emits nothing else, and the lane must not read as dead.
        | AgentEvent::TurnParked { .. }
        | AgentEvent::TurnWoken { .. } => Some(AgentStatus::Running),
        _ => None,
    }
}

/// A trace kind + short human summary for one event.
/// One trace line for a proof step — the same facts the rail folds, in the
/// order they were observed, for the reader who wants the history the rail
/// deliberately discards.
pub(super) fn trace_of(ev: &AgentEvent) -> (TraceKind, String) {
    use stella_protocol::ToolOutput;
    match ev {
        // An event from a newer stella: name it, claim nothing about it.
        AgentEvent::Unknown { event_type, .. } => {
            (TraceKind::Other, format!("unrecognized `{event_type}`"))
        }
        AgentEvent::Stage { name } => (TraceKind::Stage, format!("{name:?}").to_lowercase()),
        AgentEvent::Text { text } => (TraceKind::Text, snip(text)),
        // Mapped for completeness; `apply_event` never traces deltas (one
        // row per token would churn the capped ring — see the guard there).
        AgentEvent::TextDelta { delta } => (TraceKind::Text, snip(delta)),
        AgentEvent::Reasoning { delta } => (TraceKind::Reasoning, snip(delta)),
        AgentEvent::ToolStart { call } => (TraceKind::Tool, format!("{}()", call.name)),
        AgentEvent::SpeculationDiscarded { name, reason, .. } => {
            (TraceKind::Tool, format!("discarded {name} ({reason})"))
        }
        AgentEvent::LoopDetected {
            kind,
            repeats,
            aborted,
            ..
        } => (
            TraceKind::Other,
            format!(
                "loop {kind} ×{repeats}{}",
                if *aborted {
                    " — aborted"
                } else {
                    " — steered"
                }
            ),
        ),
        AgentEvent::BudgetDenied {
            spent_usd,
            limit_usd,
            ..
        } => (
            TraceKind::Other,
            format!("budget denied ${spent_usd:.4}/${limit_usd:.2}"),
        ),
        AgentEvent::RetriesExhausted {
            attempts,
            retryable,
            ..
        } => (
            TraceKind::Other,
            if *retryable {
                format!("retries exhausted ({attempts})")
            } else {
                format!(
                    "terminal failure, not retryable ({attempts} attempt{})",
                    if *attempts == 1 { "" } else { "s" }
                )
            },
        ),
        AgentEvent::PolicyDecision { kind, subject, .. } => {
            (TraceKind::Other, format!("policy {kind:?}: {subject}"))
        }
        AgentEvent::ToolResult {
            output,
            duration_ms,
            ..
        } => {
            let ok = matches!(output, ToolOutput::Ok { .. });
            (
                TraceKind::Tool,
                format!("{} in {duration_ms}ms", if ok { "ok" } else { "err" }),
            )
        }
        AgentEvent::FileChange {
            path,
            kind,
            added,
            removed,
            ..
        } => (
            TraceKind::File,
            format!("{kind:?} {path} +{added}/-{removed}").to_lowercase(),
        ),
        AgentEvent::BudgetTick { spent_usd, .. } => (TraceKind::Budget, format!("${spent_usd:.4}")),
        AgentEvent::StepUsage {
            model, cost_usd, ..
        } => (TraceKind::Budget, format!("{model} ${cost_usd:.4}")),
        AgentEvent::ContextRecall { frames, tokens, .. } => (
            TraceKind::Context,
            format!("{} frames, {tokens} tok", frames.len()),
        ),
        AgentEvent::ContextWrite {
            upserts,
            superseded,
            ..
        } => (TraceKind::Context, format!("+{upserts} ~{superseded}")),
        // Receipts are filtered out of the trace ring above (apply_event's
        // guard); these arms exist only to keep this mapping total.
        AgentEvent::BlockRegistered { kind, .. } => {
            (TraceKind::Context, format!("block {kind:?}").to_lowercase())
        }
        AgentEvent::StepManifest { step, blocks, .. } => (
            TraceKind::Context,
            format!("manifest step {step}: {} blocks", blocks.len()),
        ),
        // Traced under Verdict, the kind that already means "what this run
        // established": the steps and the verdict are one story, and the trace
        // log is where a reader reconstructs how the rail got where it is.
        AgentEvent::Proof { step } => (TraceKind::Verdict, crate::proof::proof_trace(step)),
        AgentEvent::Verdict { passed, .. } => (
            TraceKind::Verdict,
            if *passed {
                "passed".into()
            } else {
                "failed".into()
            },
        ),
        AgentEvent::GoalVerdict { met, round, .. } => (
            TraceKind::Verdict,
            format!("round {round} {}", if *met { "met" } else { "unmet" }),
        ),
        AgentEvent::MediaProgress { kind, .. } => {
            (TraceKind::Media, format!("{kind:?}").to_lowercase())
        }
        AgentEvent::MediaComplete { artifact } => (TraceKind::Media, artifact.label.clone()),
        AgentEvent::Commit { message, .. } => (TraceKind::Vcs, snip(message)),
        AgentEvent::Pr { status, .. } => (TraceKind::Vcs, format!("pr {status:?}").to_lowercase()),
        AgentEvent::TaskUpdate { tasks } => {
            let done = tasks.iter().filter(|t| !t.status.is_open()).count();
            (TraceKind::Other, format!("tasks {done}/{}", tasks.len()))
        }
        // A sub-agent bracket is the only trace of a child turn — its own
        // events are filtered at the parent boundary — so it names the child
        // and, on the way out, what the parent saved by not carrying its work.
        AgentEvent::SubAgent { phase } => {
            use stella_protocol::SubAgentPhase;
            (
                TraceKind::Other,
                match phase {
                    SubAgentPhase::Started { agent_id, .. } => format!("sub-agent {agent_id} ↴"),
                    SubAgentPhase::Finished {
                        agent_id,
                        status,
                        absorbed_messages,
                        ..
                    } => format!(
                        "sub-agent {agent_id} {} ({absorbed_messages} msgs absorbed)",
                        format!("{status:?}").to_lowercase()
                    ),
                },
            )
        }
        AgentEvent::ProviderFallback { from, to, .. } => {
            (TraceKind::Other, format!("fallback {from}→{to}"))
        }
        AgentEvent::Retry { attempt, .. } => (TraceKind::Other, format!("retry #{attempt}")),
        AgentEvent::Steered { text } => (
            TraceKind::Other,
            format!("steer: {}", text.chars().take(40).collect::<String>()),
        ),
        AgentEvent::TurnParked {
            description,
            poll_interval_secs,
            deadline_secs,
        } => (
            TraceKind::Other,
            format!(
                "parked: {} (every {poll_interval_secs}s, up to {deadline_secs}s)",
                description.chars().take(40).collect::<String>()
            ),
        ),
        AgentEvent::TurnWoken { reason, polls_used } => (
            TraceKind::Other,
            format!("woke: {reason} after {polls_used} probes"),
        ),
        AgentEvent::Compaction {
            before_tokens,
            after_tokens,
            ..
        } => (
            TraceKind::Other,
            format!("compact {before_tokens}→{after_tokens}"),
        ),
        AgentEvent::UsageIncomplete { reason, .. } => {
            (TraceKind::Other, format!("usage incomplete: {reason:?}"))
        }
        AgentEvent::ScopeReview { proposal } => (TraceKind::Stage, snip(&proposal.summary)),
        AgentEvent::HunkReview { proposal } => (
            TraceKind::Stage,
            format!(
                "review {} hunk{} from {}",
                proposal.hunks.len(),
                if proposal.hunks.len() == 1 { "" } else { "s" },
                proposal.tool
            ),
        ),
        AgentEvent::AskUser { question, .. } => (TraceKind::Other, snip(question)),
        AgentEvent::Error { message, .. } => (TraceKind::Error, snip(message)),
        AgentEvent::Complete { model, cost_usd } => {
            (TraceKind::Complete, format!("{model} ${cost_usd:.4}"))
        }
    }
}

/// A one-line, length-capped snip of free text for a trace row.
pub(super) fn snip(text: &str) -> String {
    const MAX: usize = 80;
    let flat = text.replace(['\n', '\r'], " ");
    let flat = flat.trim();
    if flat.chars().count() <= MAX {
        flat.to_string()
    } else {
        let head: String = flat.chars().take(MAX - 1).collect();
        format!("{head}…")
    }
}

