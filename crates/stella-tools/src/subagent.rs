// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `task` tool — the model's handle on the sub-agent primitive
//! (`stella_core::subagent`, #922).
//!
//! Delegate a bounded piece of research to a child turn that reads whatever
//! it needs and hands back a paragraph. The child's forty tool results never
//! enter this conversation, so they are never re-sent on the next step, or
//! the one after that, for the rest of the session.
//!
//! # Why a slot rather than a constructor argument
//!
//! Running a child needs a provider, a sleeper, a tool set and a budget. The
//! tool set *is* the [`crate::ToolRegistry`] this tool lives in, so a
//! dispatcher owning one and a registry owning the dispatcher would be a
//! reference cycle. Instead the registry holds a
//! [`DispatcherSlot`] the host fills after construction
//! ([`crate::ToolRegistry::attach_sub_agent_dispatcher`]) — the same
//! late-attachment shape as the hook bus and the event sender. An unfilled
//! slot is not a panic: the tool reports that sub-agents are unavailable and
//! the model does the work itself.
//!
//! # Depth is structural, not a counter
//!
//! This tool advertises `read_only: false` — truthfully, since it spends
//! money — and every child runs behind
//! [`stella_core::ports::ReadOnlyTools`]. A child therefore cannot see this
//! tool in its schema list and cannot execute it if it guesses the name.
//! Nesting is capped at one level by construction, with no depth counter to
//! thread through concurrent sibling spawns and get wrong.
//! [`stella_core::MAX_SUB_AGENT_DEPTH`] remains the primitive's own cap for
//! programmatic callers (the goal loop's verifier is one).
//!
//! # Spend
//!
//! The dispatcher pushes each child's cost to the
//! [`SubAgentSpendLedger`](stella_core::subagent::SubAgentSpendLedger) the
//! registry exposes through `ToolExecutor::drain_sub_agent_spend_usd`, which
//! the engine folds into the parent's budget at the next step boundary. That
//! ordering is what keeps `--budget` a hard ceiling: a tool cannot charge the
//! guard directly, because the engine holds it mutably for the whole turn.
//!
//! This tool deliberately does **not** charge. It used to, on the line after
//! `dispatch().await` — which never runs when the parent's turn is hard
//! cancelled mid-call, leaving a child's real spend in no ledger at all.
//! Settling belongs to whoever is still running when that happens, which is
//! the child's own thread.
//!
//! # Interruption
//!
//! The dispatcher is session-scoped; the seams that pause and stop a turn
//! are turn-scoped. [`TurnControlsSlot`] is the join: the driver publishes
//! its gate and steering tap for the duration of the turn
//! ([`crate::ToolRegistry::attach_turn_controls`]), and the dispatcher reads
//! them when a `task` call arrives. Without it, Esc ended the parent while
//! its children spent on.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde_json::{Value, json};
use stella_core::ports::TurnControls;
use stella_core::subagent::{SubAgentDispatcher, SubAgentOutcome, SubAgentSpec};
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::registry::Tool;

/// The host-filled dispatcher handle. `None` until
/// [`crate::ToolRegistry::attach_sub_agent_dispatcher`] runs — see the module
/// docs on why this cannot be a constructor argument.
pub type DispatcherSlot = Arc<RwLock<Option<Arc<dyn SubAgentDispatcher>>>>;

/// Where the running turn publishes its pause gate and steering tap for the
/// dispatcher to read. Empty between turns.
pub type TurnControlsSlot = Arc<RwLock<TurnControls>>;

/// Keeps a turn's [`TurnControls`] published for as long as it lives, and
/// clears them on drop.
///
/// Returned by [`crate::ToolRegistry::attach_turn_controls`]; hold it for the
/// turn and let it fall out of scope. Owning its slot handle rather than
/// borrowing the registry means a driver can park it wherever the turn's
/// scope actually is, including across an `await`.
///
/// A guard rather than a bare setter, and the asymmetry with
/// [`crate::ToolRegistry::attach_events`] is deliberate: a stale event sender
/// is inert (it writes to a channel nobody reads), but a stale
/// [`stella_core::ports::TurnSteering`] is *armed*. `soft_stop_requested`
/// latches by contract, so a tap left published past a stopped turn would
/// stop every child of the next turn at its first boundary.
///
/// Clearing on drop — rather than on an explicit detach call — is the same
/// argument as `stella_core::subagent::AgentAttribution`: the turn future can
/// be dropped mid-flight by a hard cancel or a panic in a tool, and the
/// controls must come down on those paths too.
#[must_use = "the turn's controls detach the moment this guard is dropped"]
pub struct TurnControlsGuard {
    slot: TurnControlsSlot,
}

impl TurnControlsGuard {
    /// Publish `controls` into `slot` until the guard drops.
    ///
    /// Replaces whatever was there rather than nesting. Turns do not overlap
    /// on one registry — every driver that runs turns concurrently (deck
    /// worker lanes, fleet workers, Best-of-N candidates) builds a registry
    /// per lane — so a stack of guards would model a state that cannot
    /// happen, at the cost of a leak whenever one was dropped out of order.
    pub(crate) fn attach(slot: &TurnControlsSlot, controls: TurnControls) -> Self {
        *slot.write().unwrap_or_else(|p| p.into_inner()) = controls;
        Self { slot: slot.clone() }
    }
}

impl Drop for TurnControlsGuard {
    fn drop(&mut self) {
        *self.slot.write().unwrap_or_else(|p| p.into_inner()) = TurnControls::none();
    }
}

/// Ceiling on the characters a child may hand back.
///
/// ~2k tokens: a substantial finding with a code excerpt, and an order of
/// magnitude less than the transcript it replaces. The cap is the whole point
/// — a child that could return anything would just move the context cost
/// rather than remove it — so it is not model-settable.
const REPORT_CHARS: usize = 8_000;

/// Ceiling on a child's model calls. Enough for a real search-and-read task;
/// small enough that a confused child cannot become a work session.
const MAX_STEPS: usize = 16;

/// The system prompt every child gets. Written to produce a *finding*, not a
/// narration: the parent is paying for the answer, not the journey.
const CHILD_SYSTEM_PROMPT: &str = "You are a research sub-agent. You have been given one \
     specific question by a parent agent that cannot see anything you do — only your final \
     message reaches it. Use your read-only tools to investigate thoroughly; being exhaustive \
     costs the parent nothing, because your intermediate work is discarded. Then answer in \
     one dense paragraph (plus a short code excerpt or file:line list where that IS the \
     answer). Report what you FOUND, with concrete paths and identifiers — never what you \
     did, and never a plan. If you could not determine the answer, say so plainly and state \
     what you ruled out; a confident wrong answer is far worse than an honest gap.";

/// `task` — delegate a bounded research question to a read-only sub-agent.
pub struct SpawnSubAgent {
    dispatcher: DispatcherSlot,
}

impl SpawnSubAgent {
    #[must_use]
    pub fn new(dispatcher: DispatcherSlot) -> Self {
        Self { dispatcher }
    }
}

#[async_trait]
impl Tool for SpawnSubAgent {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "task".into(),
            description: "Delegate a self-contained research question to a sub-agent that \
                 investigates with read-only tools and returns only its findings. Its \
                 intermediate work never enters this conversation, so use it when answering \
                 would otherwise mean reading many files you do not need to keep — 'which of \
                 these modules defines X', 'how is Y wired end to end', 'find every caller of \
                 Z and summarize the patterns'. Prefer it over running the same searches \
                 yourself whenever the evidence is bulky and only the conclusion matters. Not \
                 for work that must edit files (the sub-agent cannot write), and not for a \
                 single lookup you already know the location of — one read_file is cheaper \
                 than a sub-agent."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "A 3-6 word label for this delegation, e.g. \
                             'find retry policy'."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The question, stated in full. The sub-agent sees ONLY \
                             this — it cannot read the conversation, so include every name, \
                             path and constraint it needs, and say exactly what a good answer \
                             contains."
                    }
                },
                "required": ["description", "prompt"],
                "additionalProperties": false
            }),
            // Truthfully not read-only: it spends money. This is also what
            // makes nesting structural — children run behind `ReadOnlyTools`,
            // so they never see this tool. See the module docs.
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, _root: &std::path::Path) -> ToolOutput {
        let prompt = match input.get("prompt").and_then(Value::as_str) {
            Some(prompt) if !prompt.trim().is_empty() => prompt.trim(),
            _ => {
                return ToolOutput::Error {
                    message: "missing required string field `prompt` — the sub-agent cannot \
                         see this conversation, so the question must be self-contained"
                        .into(),
                };
            }
        };
        let description = input
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .unwrap_or("sub-agent");

        let dispatcher = {
            let slot = self.dispatcher.read().unwrap_or_else(|p| p.into_inner());
            slot.clone()
        };
        let Some(dispatcher) = dispatcher else {
            return ToolOutput::Error {
                message: "sub-agents are unavailable in this session — do the work directly \
                     with your own tools"
                    .into(),
            };
        };

        let spec = SubAgentSpec {
            agent_id: slug(description),
            system_prompt: Some(CHILD_SYSTEM_PROMPT.to_string()),
            instruction: prompt.to_string(),
            max_steps: MAX_STEPS,
            max_report_chars: REPORT_CHARS,
            // Write access is deliberately not model-settable: this tool
            // delegates *research*. A child that could edit would need the
            // parent's review gates around it, which is its own change.
            write_access: false,
            depth: 1,
            ..SubAgentSpec::default()
        };

        // Already settled by the time this resolves — the dispatcher charges
        // the ledger from the child's own thread, which is the only place
        // that still runs when a hard cancel means this `await` never
        // resumes. Charging here too would double-bill.
        let outcome = dispatcher.dispatch(spec).await;
        render(&outcome)
    }
}

/// Turn a child's outcome into a model-visible result.
///
/// An incomplete child is an `Ok` carrying its partial finding plus a plain
/// statement of what stopped it — not an `Error`. The distinction matters:
/// the salvaged text is real evidence the parent paid for, and burying it in
/// an error message invites the model to discard it and redo the work.
/// A child that never ran IS an error, because there is nothing to use.
fn render(outcome: &SubAgentOutcome) -> ToolOutput {
    match outcome {
        SubAgentOutcome::Completed(report) => ToolOutput::Ok {
            content: if report.truncated {
                format!(
                    "{}\n\n[report truncated at {} characters — ask a narrower question if \
                     the answer was cut off]",
                    report.summary, REPORT_CHARS
                )
            } else {
                report.summary.clone()
            },
        },
        SubAgentOutcome::Incomplete { report, reason } if !report.summary.is_empty() => {
            ToolOutput::Ok {
                content: format!(
                    "{}\n\n[partial: the sub-agent stopped before finishing — {reason}. Treat \
                     the above as incomplete evidence, not a final answer.]",
                    report.summary
                ),
            }
        }
        SubAgentOutcome::Incomplete { reason, .. } => ToolOutput::Error {
            message: format!(
                "the sub-agent stopped before producing anything: {reason} — do the work \
                 directly, or ask a narrower question"
            ),
        },
        SubAgentOutcome::Refused { reason } => ToolOutput::Error {
            message: format!(
                "the sub-agent was not started: {reason} — do the work directly with your \
                 own tools"
            ),
        },
    }
}

/// A short, stable, filesystem-safe id from the model's description, for
/// event attribution. Lowercase alphanumerics and dashes; never empty.
fn slug(description: &str) -> String {
    let mut out = String::new();
    for ch in description.chars() {
        if ch.is_ascii_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 32 {
            break;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "sub-agent".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
#[path = "subagent/tests.rs"]
mod tests;
