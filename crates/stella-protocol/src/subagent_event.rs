// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The sub-agent lifecycle payload carried by [`AgentEvent::SubAgent`](crate::AgentEvent::SubAgent).
//!
//! A sub-agent is a bounded child turn with its own transcript and its own
//! carved budget (`stella_core::subagent`). Its intermediate work never
//! reaches the parent's message history — that is the whole point — so the
//! event stream is the ONLY place a consumer can see that a child ran, what
//! it cost, and how it ended.
//!
//! # Why one variant with a nested phase, not two variants
//!
//! Mirrors [`ProofStep`](crate::ProofStep): every downstream exhaustive
//! matcher (`replay::event_signature`, the TUI's `Model::apply`,
//! `event_line`, `trace_of`) gets ONE arm to write instead of two, and the
//! start/finish pair can never drift out of sync because they share a type.
//!
//! # What is deliberately NOT here
//!
//! The child's own `Stage`, `Complete`, `Text`, `TextDelta`, `Reasoning`
//! and `BudgetTick` events are dropped at the child/parent boundary rather
//! than re-tagged (see `stella_core::subagent`'s event contract):
//!
//! - `Stage`/`Complete` — the parent (or the pipeline above it) is the sole
//!   authority for stage boundaries and terminal events. A child's copies
//!   would be read as the parent's own, which is exactly the
//!   "must not be mistaken for the parent's stage boundaries" hazard.
//! - `Text`/`TextDelta`/`Reasoning` — the child's narration is a *draft* of
//!   its report. The report is delivered exactly once, in
//!   [`SubAgentPhase::Finished::summary`]; streaming it as well would both
//!   duplicate it and interleave child prose with the parent's answer.
//! - `BudgetTick` — a tick reports the emitting guard's numbers, and the
//!   child's guard holds the *carve*, not the session. Forwarding it would
//!   make a HUD show `$0.02 / $0.10` mid-turn for a session at `$3 / $10`.
//!   The parent re-ticks its own (post-settlement) numbers instead.
//!
//! Everything else — `ToolStart`, `ToolResult`, `StepUsage`,
//! `UsageIncomplete`, `FileChange`, `Retry`, `LoopDetected`,
//! `BudgetDenied`, context receipts — IS forwarded. `StepUsage` in
//! particular must be: it is the metering record, so dropping it would make
//! child spend vanish from `stella stats` and quietly falsify the
//! `$/resolved task` number.

use serde::{Deserialize, Serialize};

/// How a sub-agent's turn ended. The parent reasons about this as data — a
/// failed child is never an error that kills the parent turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SubAgentStatus {
    /// The child produced a final answer with no further tool calls.
    Completed,
    /// The child's turn aborted cleanly at a step boundary — its carved
    /// budget, its step cap, loop detection, or exhausted retries. Any text
    /// it had already produced is salvaged into the report rather than
    /// discarded, so paid work is never thrown away.
    Incomplete,
    /// The child never ran: refused before its first model call (no budget
    /// headroom left in the parent's carve, or the nesting-depth cap). Cost
    /// is always exactly zero.
    Refused,
}

impl SubAgentStatus {
    /// Whether the child reached a clean final answer. Distinguishes the one
    /// status a caller can treat as authoritative from the two it must
    /// hedge on.
    #[must_use]
    pub fn is_completed(&self) -> bool {
        matches!(self, SubAgentStatus::Completed)
    }
}

/// One point in a sub-agent's lifecycle. Exactly one `Started` and exactly
/// one `Finished` are emitted per spawn, in that order, even when the child
/// is refused before its first model call (a refusal still brackets, so a
/// consumer folding these never sees an unclosed child).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum SubAgentPhase {
    /// A child turn is about to start. Every event between this and the
    /// matching `Finished` with the same `agent_id` belongs to the child,
    /// not the parent — this bracket IS the attribution mechanism, which is
    /// why no per-event agent field was added to the other 35 variants.
    Started {
        /// Stable id for this child, unique within the parent turn.
        agent_id: String,
        /// The child's task, truncated for display. Never the full prompt —
        /// the prompt can be large and the event stream is journaled.
        instruction_preview: String,
        /// The USD ceiling actually carved for this child, after clamping
        /// the request against the parent's remaining headroom. `None` when
        /// no axis anywhere bounds it.
        budget_usd: Option<f64>,
        /// Whether the child may mutate the workspace. `false` (the
        /// default) means it ran behind a read-only view of the parent's
        /// tools and could not have changed anything.
        write_access: bool,
        /// Nesting depth: `1` for a child of the top-level turn.
        depth: u8,
    },
    /// The child turn ended. Emitted on every path, including a refusal.
    Finished {
        /// Matches the `Started` this closes.
        agent_id: String,
        status: SubAgentStatus,
        /// The report handed back to the parent — the ONLY thing that may
        /// enter the parent's transcript, and already clamped to the spec's
        /// character cap.
        summary: String,
        /// Whether the report was cut to fit that cap. Never silent: a
        /// consumer can tell an exhaustive answer from a clipped one.
        truncated: bool,
        /// The child's total spend, already settled into the parent's guard
        /// by the time this is emitted.
        cost_usd: f64,
        /// Model calls the child made.
        steps: usize,
        /// How many messages the child's private transcript grew to — the
        /// context the parent did NOT have to carry. This is the primitive's
        /// whole value proposition, reported rather than asserted.
        absorbed_messages: usize,
        /// Present only when `status` is not `Completed`: why it ended.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl SubAgentPhase {
    /// The child this phase belongs to, for folding a stream into per-agent
    /// state without matching the variant.
    #[must_use]
    pub fn agent_id(&self) -> &str {
        match self {
            SubAgentPhase::Started { agent_id, .. } | SubAgentPhase::Finished { agent_id, .. } => {
                agent_id
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_roundtrips_with_a_snake_case_discriminant() {
        let started = SubAgentPhase::Started {
            agent_id: "search-1".into(),
            instruction_preview: "find the retry policy".into(),
            budget_usd: Some(0.25),
            write_access: false,
            depth: 1,
        };
        let json = serde_json::to_string(&started).unwrap();
        assert!(json.contains("\"phase\":\"started\""), "{json}");
        let back: SubAgentPhase = serde_json::from_str(&json).unwrap();
        assert_eq!(back, started);
    }

    #[test]
    fn a_completed_finish_omits_the_reason_field_entirely() {
        // `reason` is the "why did this not finish" field; a clean finish
        // must not carry a null for it, or every consumer has to special-case
        // the empty case.
        let finished = SubAgentPhase::Finished {
            agent_id: "search-1".into(),
            status: SubAgentStatus::Completed,
            summary: "retry policy lives in retry.rs".into(),
            truncated: false,
            cost_usd: 0.004,
            steps: 3,
            absorbed_messages: 9,
            reason: None,
        };
        let json = serde_json::to_string(&finished).unwrap();
        assert!(!json.contains("reason"), "{json}");
        assert_eq!(
            serde_json::from_str::<SubAgentPhase>(&json).unwrap(),
            finished
        );
    }

    #[test]
    fn agent_id_reads_from_either_phase() {
        let started = SubAgentPhase::Started {
            agent_id: "a".into(),
            instruction_preview: String::new(),
            budget_usd: None,
            write_access: false,
            depth: 1,
        };
        let finished = SubAgentPhase::Finished {
            agent_id: "a".into(),
            status: SubAgentStatus::Refused,
            summary: String::new(),
            truncated: false,
            cost_usd: 0.0,
            steps: 0,
            absorbed_messages: 0,
            reason: Some("no budget headroom".into()),
        };
        assert_eq!(started.agent_id(), finished.agent_id());
    }

    #[test]
    fn only_completed_is_authoritative() {
        assert!(SubAgentStatus::Completed.is_completed());
        assert!(!SubAgentStatus::Incomplete.is_completed());
        assert!(!SubAgentStatus::Refused.is_completed());
    }
}
