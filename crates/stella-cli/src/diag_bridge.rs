// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The domain bridge — `docs/spec/diagnostics.md` §8.
//!
//! The diagnostic plane must **not** restate `AgentEvent`. A second stream
//! carrying what the domain plane already carries would be a weaker parallel
//! authority. So this adapter subscribes to the existing event stream and
//! emits diagnostic records that carry **only a sequence number and a shape** —
//! never a payload:
//!
//! - `AgentEvent::TextDelta` → **no record**. Counted into a bounded tally,
//!   exactly as `stella-serve::observe::tally` concluded after building the
//!   alternative and rejecting it for putting model output in a log (§3.8).
//! - `AgentEvent::ToolStart` → `{code:"agent.tool.call", seq, tool, args_bytes}`.
//!
//! One merged, ordered timeline for an operator; one authority for replay.
//!
//! ## Why this is worth more than instrumenting the crates directly
//!
//! Almost everything that explains a bad run already crosses this seam as a
//! typed value: `Compaction` is fourteen numbers, `StepUsage` carries tokens,
//! cost, duration and retry count, `ToolResult` carries a duration and an
//! error bit, `LoopDetected` carries its repeat count, `BudgetDenied` carries
//! spend against limit. None of that needs a new emit site in `stella-core`,
//! and adding one would violate §7.3 anyway.
//!
//! ## The match is exhaustive on purpose
//!
//! `stella-protocol/src/event.rs` carries a checklist of every downstream
//! matcher a new variant must be considered against, and it distinguishes
//! *compile-enforced* matchers from *silent* ones that would quietly ignore a
//! new variant. This one is compile-enforced: there is no `_` arm, so adding
//! an `AgentEvent` variant fails to build here until someone decides whether it
//! is a record, a tally, or deliberately nothing. A wildcard would make new
//! events silently invisible in exactly the runs this exists to explain.
//!
//! ## What must never reach a record
//!
//! Every field below is either a number, a bool, a closed enum, or a
//! `PathClass`. Content-bearing fields — `Text.delta`, `TextDelta.text`,
//! `Reasoning.delta`, `Steered.text`, `ToolCall.input`, `ToolOutput`'s content
//! and message, `StepUsage.output_text`, `BlockRegistered.content`,
//! `FileChange.diff`, `AskUser.question`, `GoalVerdict.reasoning`,
//! `LoopDetected.evidence`, `Error.message`, `Unknown.payload` — are read for
//! their *length* at most, and §5.2 makes any slip a compile error rather than
//! a review question.

use std::sync::Arc;

use stella_diag::{
    Cx, Dx, Fields, Level, PathClass, PathContext, Record, Redacted, ShortId, log_enum, note,
};
use stella_protocol::{
    AgentEvent, BudgetScope, FileChangeKind, ModelCallRole, PolicyKind, StageKind, ToolOutput,
    UsageIncompleteReason,
};

/// This module's `module_path!()`, so every record it emits filters under the
/// same target regardless of which helper built it.
const TARGET: &str = "stella::diag_bridge";

log_enum! {
    /// Which built-in tool a call named.
    ///
    /// A closed vocabulary, not the raw string: a tool name usually comes from
    /// the registry, but the *model* chooses what to ask for and can name
    /// something that does not exist — at which point the "name" is model
    /// output wearing an identifier's clothes. The built-ins answer the
    /// question a benchmark actually asks (was it shelling out or editing?),
    /// and everything else is [`ToolName::Other`].
    pub enum ToolName {
        Bash => "bash",
        ReadFile => "read_file",
        WriteFile => "write_file",
        EditFile => "edit_file",
        ApplyEdits => "apply_edits",
        DeleteFile => "delete_file",
        Glob => "glob",
        Grep => "grep",
        VerifyDone => "verify_done",
        SaveMemory => "save_memory",
        GenerateImage => "generate_image",
        WebFetch => "web_fetch",
        Task => "task",
        /// An MCP tool, a custom script tool, or a name the model invented.
        Other => "other",
    }
}

impl ToolName {
    fn classify(name: &str) -> Self {
        match name {
            "bash" => Self::Bash,
            "read_file" => Self::ReadFile,
            "write_file" => Self::WriteFile,
            "edit_file" => Self::EditFile,
            "apply_edits" => Self::ApplyEdits,
            "delete_file" => Self::DeleteFile,
            "glob" => Self::Glob,
            "grep" => Self::Grep,
            "verify_done" => Self::VerifyDone,
            "save_memory" => Self::SaveMemory,
            "generate_image" => Self::GenerateImage,
            "web_fetch" => Self::WebFetch,
            "task" => Self::Task,
            _ => Self::Other,
        }
    }
}

/// The bounded tally §3.8 asks for, in place of per-token records.
///
/// A 10k-token turn produces ten thousand `TextDelta`s. Recording each would
/// put model output in a log *and* make the crash ring a window onto the last
/// two seconds of one answer. Counting them costs eight words and answers the
/// only questions an operator actually has: how much came back, and was any of
/// it reasoning?
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Tally {
    text_deltas: u64,
    text_bytes: u64,
    reasoning_deltas: u64,
    reasoning_bytes: u64,
    blocks_registered: u64,
    budget_ticks: u64,
    events: u64,
}

impl Tally {
    fn fields(&self) -> Fields {
        Fields::new()
            .with("events", self.events)
            .with("text_deltas", self.text_deltas)
            .with("text_bytes", self.text_bytes)
            .with("reasoning_deltas", self.reasoning_deltas)
            .with("reasoning_bytes", self.reasoning_bytes)
            .with("blocks_registered", self.blocks_registered)
            .with("budget_ticks", self.budget_ticks)
    }
}

/// Turns the domain event stream into a diagnostic timeline.
///
/// Fed from the **receiving** side of the event channel, never the sending
/// side. Holding an `EventSender` clone would keep the channel open forever and
/// hang `stella run` at close-out (the teardown discipline
/// `agent::persistence::close_event_stream` exists to enforce); observing
/// received events cannot.
pub(crate) struct DomainBridge {
    dx: Arc<Dx>,
    /// This bridge's own monotonic ordinal over **every** event it saw,
    /// including the ones that produce no record.
    ///
    /// Deliberately not shared with the renderer's or the forwarder's `seq`:
    /// those skip `TextDelta` and stop advancing when there is no store, so
    /// neither is the raw stream ordinal. A reader correlating a record back to
    /// the domain plane needs the ordinal that actually counts events.
    seq: u64,
    cx: Cx,
    tally: Tally,
    paths: PathContext,
}

impl DomainBridge {
    pub(crate) fn new(dx: Arc<Dx>, workspace_root: Option<std::path::PathBuf>) -> Self {
        Self {
            dx,
            seq: 0,
            cx: Cx::EMPTY,
            tally: Tally::default(),
            paths: PathContext::detect(workspace_root),
        }
    }

    fn emit(&self, level: Level, code: &'static str, fields: Fields) {
        self.dx
            .emit(Record::new(level, code, TARGET, self.cx, fields));
    }

    /// A record carrying the stream ordinal and nothing else yet.
    fn at_seq(&self) -> Fields {
        Fields::new().with("seq", self.seq)
    }

    /// Fold one event into the timeline.
    ///
    /// Cheap by construction: the expensive variants are the frequent ones, and
    /// those only increment a counter.
    pub(crate) fn observe(&mut self, event: &AgentEvent) {
        self.seq += 1;
        self.tally.events += 1;

        match event {
            // ---- Counted, never recorded (§3.8). ------------------------
            AgentEvent::TextDelta { text } => {
                self.tally.text_deltas += 1;
                self.tally.text_bytes += text.len() as u64;
            }
            AgentEvent::Reasoning { delta } => {
                self.tally.reasoning_deltas += 1;
                self.tally.reasoning_bytes += delta.len() as u64;
            }
            // One per newly-eligible context block — every tool result, every
            // assistant message, every recall frame. High enough volume to
            // swamp a ring, and it is the one variant that can carry raw
            // prompt text.
            AgentEvent::BlockRegistered { .. } => self.tally.blocks_registered += 1,
            // Fires on every money-spending call; the running total is a
            // metric, and §2.1 says a metric is not a log line. The decision
            // that matters — a denial — has its own variant below.
            AgentEvent::BudgetTick { .. } => self.tally.budget_ticks += 1,
            // The whole answer, once per step. Its length is the only part of
            // it that is not content.
            AgentEvent::Text { delta } => {
                self.tally.text_bytes += delta.len() as u64;
            }

            // ---- The model call: tokens, cost, latency, retries. ---------
            AgentEvent::StepManifest {
                turn_instance,
                step,
                call_seq,
                role,
                blocks,
                effective_budget_tokens,
                estimated_input_tokens,
                ..
            } => {
                // The only variant carrying all three of turn, step and
                // call_seq, so it is what advances the correlation context for
                // everything recorded after it.
                self.cx.turn = Some(u64::from(*turn_instance));
                self.cx.step = Some(*step as u64);
                self.emit(
                    Level::Debug,
                    "agent.step.manifest",
                    self.at_seq()
                        .with("call_seq", *call_seq)
                        .with("role", role_name(*role))
                        .with("blocks", blocks.len())
                        .with("effective_budget_tokens", *effective_budget_tokens)
                        .with("estimated_input_tokens", *estimated_input_tokens),
                );
            }
            AgentEvent::StepUsage {
                step,
                role,
                provider,
                model,
                input_tokens,
                output_tokens,
                cached_input_tokens,
                cache_write_tokens,
                cost_usd,
                duration_ms,
                retries,
                tool_calls,
                complete,
                ..
            } => {
                self.emit(
                    Level::Debug,
                    "agent.step.usage",
                    self.at_seq()
                        .with("step", *step)
                        .with("role", role_name(*role))
                        .with("provider", operator_id(provider))
                        .with("model", operator_id(model))
                        .with("input_tokens", *input_tokens)
                        .with("output_tokens", *output_tokens)
                        .with("cached_input_tokens", *cached_input_tokens)
                        .with("cache_write_tokens", *cache_write_tokens)
                        .with("cost_usd", *cost_usd)
                        .with("duration_ms", *duration_ms)
                        .with("retries", *retries)
                        .with("tool_calls", *tool_calls)
                        .with("complete", *complete),
                );
            }
            // A model call that spent time and returned nothing usable. On a
            // benchmark this is the difference between "the agent was slow"
            // and "the provider ate ninety seconds and failed".
            AgentEvent::UsageIncomplete {
                role,
                provider,
                model,
                reason,
                duration_ms,
                retries,
                partial,
            } => {
                // Token counts are content-free, so the recovered figures ride
                // the diagnostic timeline like any other operator id. They are
                // what turns this line from "an attempt failed" into "an
                // attempt failed and here is what it cost" — the question an
                // operator reading a run's timeline actually has.
                let mut fields = self
                    .at_seq()
                    .with("role", role_name(*role))
                    .with("provider", operator_id(provider))
                    .with("model", operator_id(model))
                    .with("reason", usage_incomplete_reason(*reason))
                    .with("duration_ms", *duration_ms)
                    .with("retries", *retries)
                    .with("recovered", partial.is_some());
                if let Some(partial) = partial {
                    fields = fields
                        .with("recovered_input_tokens", partial.usage.input_tokens)
                        .with("recovered_output_tokens", partial.usage.output_tokens)
                        .with("recovered_cached_tokens", partial.usage.cached_input_tokens)
                        .with("recovered_cost_usd", partial.cost_usd)
                        .with("input_reported", partial.input_reported);
                }
                self.emit(Level::Warn, "agent.step.usage_incomplete", fields);
            }
            // `reason` is `error.to_string()` — a provider's own prose, and
            // exactly the string §5.3 exists to keep out. The attempt number is
            // the part that explains anything.
            AgentEvent::Retry { attempt, .. } => {
                self.emit(
                    Level::Warn,
                    "agent.model.retry",
                    self.at_seq().with("attempt", *attempt),
                );
            }
            AgentEvent::RetriesExhausted {
                turn_instance,
                attempts,
                retryable,
                ..
            } => {
                self.emit(
                    Level::Error,
                    "agent.model.retries_exhausted",
                    self.at_seq()
                        .with("turn", u64::from(*turn_instance))
                        .with("attempts", *attempts)
                        .with("retryable", *retryable),
                );
            }
            AgentEvent::ProviderFallback { from, to, .. } => {
                self.emit(
                    Level::Warn,
                    "agent.provider.fallback",
                    self.at_seq()
                        .with("from", operator_id(from))
                        .with("to", operator_id(to)),
                );
            }

            // ---- Tools. -------------------------------------------------
            AgentEvent::ToolStart { call } => {
                self.emit(
                    Level::Debug,
                    "agent.tool.call",
                    self.at_seq()
                        .with("tool", ToolName::classify(&call.name))
                        // The design's own example field. `call.input` is
                        // arbitrary model-produced JSON; its size is a useful
                        // signal and its bytes are not ours to record.
                        .with("args_bytes", call.input.to_string().len()),
                );
            }
            AgentEvent::ToolResult {
                output,
                duration_ms,
                speculated,
                ..
            } => {
                let (ok, bytes) = match output {
                    ToolOutput::Ok { content } => (true, content.len()),
                    ToolOutput::Error { message } => (false, message.len()),
                };
                self.emit(
                    if ok { Level::Debug } else { Level::Warn },
                    "agent.tool.result",
                    self.at_seq()
                        .with("ok", ok)
                        .with("duration_ms", *duration_ms)
                        .with("speculated", *speculated)
                        .with("output_bytes", bytes),
                );
            }
            AgentEvent::SpeculationDiscarded { .. } => {
                self.emit(
                    Level::Debug,
                    "agent.tool.speculation_discarded",
                    self.at_seq(),
                );
            }

            // ---- Engine decisions, already typed by stella-core. ---------
            // Fourteen numbers and not one string. §7.3 says pure functions
            // return their rationale and the caller records it; this is the
            // caller, and this is the rationale.
            AgentEvent::Compaction {
                before_tokens,
                after_tokens,
                evicted,
                deduped,
                superseded,
                aged,
                summarized,
                effective_budget_tokens,
                calibration_factor,
                ..
            } => {
                self.emit(
                    Level::Info,
                    "agent.context.compaction",
                    self.at_seq()
                        .with("before_tokens", *before_tokens)
                        .with("after_tokens", *after_tokens)
                        .with("evicted", *evicted)
                        .with("deduped", *deduped)
                        .with("superseded", *superseded)
                        .with("aged", *aged)
                        .with("summarized", *summarized)
                        .with("effective_budget_tokens", *effective_budget_tokens)
                        .with("calibration_factor", *calibration_factor),
                );
            }
            AgentEvent::LoopDetected {
                turn_instance,
                pattern,
                repeats,
                aborted,
                ..
            } => {
                self.emit(
                    Level::Warn,
                    "agent.loop.detected",
                    self.at_seq()
                        .with("turn", u64::from(*turn_instance))
                        .with("pattern_len", pattern.len())
                        .with("repeats", *repeats)
                        .with("aborted", *aborted),
                );
            }
            AgentEvent::BudgetDenied {
                scope,
                spent_usd,
                limit_usd,
                ..
            } => {
                self.emit(
                    Level::Error,
                    "agent.budget.denied",
                    self.at_seq()
                        .with("scope", budget_scope(*scope))
                        .with("spent_usd", *spent_usd)
                        .with("limit_usd", *limit_usd),
                );
            }
            AgentEvent::PolicyDecision { kind, .. } => {
                self.emit(
                    Level::Info,
                    "agent.policy.decision",
                    self.at_seq().with("kind", policy_kind(*kind)),
                );
            }

            // ---- Stage / turn shape. ------------------------------------
            AgentEvent::Stage { name } => {
                self.emit(
                    Level::Info,
                    "agent.stage",
                    self.at_seq().with("stage", stage_name(*name)),
                );
            }
            AgentEvent::Complete { model, cost_usd } => {
                self.emit(
                    Level::Info,
                    "agent.complete",
                    self.at_seq()
                        .with("model", operator_id(model))
                        .with("cost_usd", *cost_usd),
                );
            }
            AgentEvent::GoalVerdict {
                round,
                met,
                cost_usd,
                ..
            } => {
                self.emit(
                    Level::Info,
                    "agent.goal.verdict",
                    self.at_seq()
                        .with("round", *round)
                        .with("met", *met)
                        .with("cost_usd", *cost_usd),
                );
            }
            AgentEvent::Verdict { passed, .. } => {
                self.emit(
                    Level::Info,
                    "agent.verifier.verdict",
                    self.at_seq().with("passed", *passed),
                );
            }
            AgentEvent::Error { retryable, .. } => {
                self.emit(
                    Level::Error,
                    "agent.error",
                    self.at_seq().with("retryable", *retryable),
                );
            }

            // ---- Context plane. -----------------------------------------
            AgentEvent::ContextRecall {
                frames,
                tokens,
                latency_ms,
                used_ann_index,
                ..
            } => {
                self.emit(
                    Level::Debug,
                    "agent.context.recall",
                    self.at_seq()
                        .with("frames", frames.len())
                        .with("tokens", *tokens)
                        .with("latency_ms", *latency_ms)
                        .with("used_ann_index", *used_ann_index),
                );
            }
            AgentEvent::ContextWrite {
                upserts,
                superseded,
                ..
            } => {
                self.emit(
                    Level::Debug,
                    "agent.context.write",
                    self.at_seq()
                        .with("upserts", *upserts)
                        .with("superseded", *superseded),
                );
            }

            // ---- Workspace effects. -------------------------------------
            AgentEvent::FileChange {
                path,
                kind,
                added,
                removed,
                ..
            } => {
                self.emit(
                    Level::Debug,
                    "agent.file.change",
                    self.at_seq()
                        .with("kind", file_change_kind(*kind))
                        .with(
                            "path",
                            PathClass::classify(std::path::Path::new(path), &self.paths),
                        )
                        .with("added", *added)
                        .with("removed", *removed),
                );
            }
            AgentEvent::Commit { sha, .. } => {
                self.emit(
                    Level::Info,
                    "agent.commit",
                    // A commit sha is hex and public by nature, but it is also
                    // a whole identifier; §7.2's argument for truncation
                    // applies unchanged, and eight characters is what anyone
                    // pastes into `git show` anyway.
                    self.at_seq().with("sha", ShortId::new(sha)),
                );
            }
            AgentEvent::Pr { number, .. } => {
                self.emit(
                    Level::Info,
                    "agent.pr",
                    self.at_seq().with("number", *number),
                );
            }

            // ---- Low-volume, low-signal: shape only. --------------------
            AgentEvent::Steered { .. } => {
                self.emit(Level::Info, "agent.steered", self.at_seq());
            }
            AgentEvent::Proof { .. } => {
                self.emit(Level::Debug, "agent.proof", self.at_seq());
            }
            AgentEvent::ScopeReview { .. } => {
                self.emit(Level::Info, "agent.scope.review", self.at_seq());
            }
            // Occurrence only. The proposal carries file paths and diff text,
            // both content-bearing, so nothing from it reaches the record.
            AgentEvent::HunkReview { .. } => {
                self.emit(Level::Info, "agent.hunk.review", self.at_seq());
            }
            AgentEvent::AskUser { .. } => {
                self.emit(Level::Info, "agent.ask_user", self.at_seq());
            }
            AgentEvent::MediaProgress { .. } => {
                self.emit(Level::Debug, "agent.media.progress", self.at_seq());
            }
            AgentEvent::MediaComplete { .. } => {
                self.emit(Level::Debug, "agent.media.complete", self.at_seq());
            }
            AgentEvent::TaskUpdate { tasks } => {
                self.emit(
                    Level::Debug,
                    "agent.task.update",
                    self.at_seq().with("tasks", tasks.len()),
                );
            }
            AgentEvent::SubAgent { .. } => {
                self.emit(Level::Debug, "agent.subagent", self.at_seq());
            }
            // `event_type` is externally-authored text, so it is not a field
            // and must not become a metric label either — the count is the
            // whole signal, and a non-zero one means this build is reading a
            // stream it does not fully understand.
            AgentEvent::Unknown { .. } => {
                self.emit(Level::Warn, "agent.unknown", self.at_seq());
            }
        }
    }

    /// Emit the tally. Call once, when the stream ends.
    ///
    /// This is the record that makes property 6 of §12 checkable — a turn of
    /// any length produces a record count in the number of *steps*, and the
    /// token count lives here as two integers.
    pub(crate) fn finish(&self) {
        self.emit(Level::Info, "agent.stream.tally", self.tally.fields());
    }
}

/// A provider or model identifier the **operator** configured.
///
/// Through the reviewed hatch (§5.5) rather than around the type system. These
/// are not model output and not workspace content — they come from a settings
/// file, a `--model` flag, or a provider catalog — and on a benchmark run
/// "which model was this" is the first question anyone asks of the log.
fn operator_id(id: &str) -> Redacted<String> {
    Redacted::reviewed(
        id.to_owned(),
        note!(
            "a provider/model identifier chosen by the operator in settings, on the command line, \
             or from the shipped catalog; never model output, file content, or a path — and the \
             one field a benchmark log is useless without"
        ),
    )
}

// The closed-vocabulary mappings. These are functions rather than `Loggable`
// impls because the enums belong to `stella-protocol` and the trait belongs to
// `stella-diag`, so an impl here would violate the orphan rule. Each match is
// exhaustive, so a new protocol variant fails the build until it is spelled.

fn stage_name(stage: StageKind) -> &'static str {
    match stage {
        StageKind::Triage => "triage",
        StageKind::ContextRecall => "context_recall",
        StageKind::Plan => "plan",
        StageKind::ScopeReview => "scope_review",
        StageKind::Witness => "witness",
        StageKind::Execute => "execute",
        StageKind::Verify => "verify",
        // The stage is `verdict`; the *model* that runs it is the verifier
        // (see `role_name`). Naming the stage after its model is the exact
        // `verify → verifier` adjacency #1394's rename existed to remove, so
        // this field follows the wire enum rather than lagging it (#1465).
        StageKind::Verdict => "verdict",
        StageKind::Reflect => "reflect",
        StageKind::ContextWrite => "context_write",
        StageKind::Complete => "complete",
    }
}

fn role_name(role: ModelCallRole) -> &'static str {
    match role {
        ModelCallRole::Unknown => "unknown",
        ModelCallRole::Triage => "triage",
        ModelCallRole::Plan => "plan",
        ModelCallRole::PlanRepair => "plan_repair",
        ModelCallRole::WitnessAuthor => "witness_author",
        ModelCallRole::WitnessRepair => "witness_repair",
        ModelCallRole::Worker => "worker",
        ModelCallRole::DistressGuidance => "distress_guidance",
        ModelCallRole::Verdict => "verifier",
        ModelCallRole::AgentAuthor => "agent_author",
        ModelCallRole::SkillAuthor => "skill_author",
        ModelCallRole::DomainInference => "domain_inference",
        ModelCallRole::Reflection => "reflection",
        ModelCallRole::Summarization => "summarization",
    }
}

fn usage_incomplete_reason(reason: UsageIncompleteReason) -> &'static str {
    match reason {
        UsageIncompleteReason::ProviderError => "provider_error",
        UsageIncompleteReason::Timeout => "timeout",
        UsageIncompleteReason::Cancelled => "cancelled",
    }
}

fn budget_scope(scope: BudgetScope) -> &'static str {
    match scope {
        BudgetScope::Turn => "turn",
        BudgetScope::Session => "session",
    }
}

fn policy_kind(kind: PolicyKind) -> &'static str {
    match kind {
        PolicyKind::Evaluated => "evaluated",
        PolicyKind::Blocked => "blocked",
        PolicyKind::ApprovalRequested => "approval_requested",
        PolicyKind::SecretDetected => "secret_detected",
    }
}

fn file_change_kind(kind: FileChangeKind) -> &'static str {
    match kind {
        FileChangeKind::Read => "read",
        FileChangeKind::Created => "created",
        FileChangeKind::Modified => "modified",
        FileChangeKind::Deleted => "deleted",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::{ToolCall, ToolOutput};

    fn bridge() -> (DomainBridge, Arc<stella_diag::Capture>) {
        let (dx, capture) = Dx::capturing();
        (
            DomainBridge::new(Arc::new(dx), Some(std::path::PathBuf::from("/w"))),
            capture,
        )
    }

    fn tool_call(name: &str, input: serde_json::Value) -> AgentEvent {
        AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "call-1".into(),
                name: name.into(),
                input,
            },
        }
    }

    /// The closed vocabulary keeps the project's settled distinction: the
    /// **stage** is `verdict`, the **model** that runs it is the `verifier`.
    /// Both names live in this file, one line apart, and having the stage
    /// borrow the model's name is what put the diagnostic field out of step
    /// with the wire enum it is derived from (#1465).
    #[test]
    fn the_stage_is_verdict_and_only_the_role_is_the_verifier() {
        assert_eq!(stage_name(StageKind::Verdict), "verdict");
        assert_eq!(role_name(ModelCallRole::Verdict), "verifier");
    }

    /// §3.8 and property 6 of §12: a turn of any length produces a record count
    /// in the number of steps, never the number of tokens.
    #[test]
    fn ten_thousand_text_deltas_produce_no_records() {
        let (mut bridge, records) = bridge();
        for _ in 0..10_000 {
            bridge.observe(&AgentEvent::TextDelta { text: "tok".into() });
        }
        assert!(
            records.records().is_empty(),
            "a per-token record would put model output in a log"
        );

        bridge.finish();
        let tally = records.find("agent.stream.tally").expect("a tally");
        assert_eq!(
            tally.fields.get("text_deltas"),
            Some(&stella_diag::FieldValue::Uint(10_000))
        );
        assert_eq!(
            tally.fields.get("text_bytes"),
            Some(&stella_diag::FieldValue::Uint(30_000))
        );
    }

    /// The design's own example record, §8.
    #[test]
    fn a_tool_call_records_its_shape_and_not_its_arguments() {
        let (mut bridge, records) = bridge();
        bridge.observe(&tool_call(
            "bash",
            serde_json::json!({ "command": "cat ~/.ssh/id_ed25519" }),
        ));

        let record = records.find("agent.tool.call").expect("a record");
        let json = serde_json::to_string(&record).expect("serialize");
        assert!(json.contains(r#""tool":"bash""#), "{json}");
        assert!(json.contains(r#""args_bytes""#), "{json}");
        assert!(!json.contains("id_ed25519"), "arguments leaked: {json}");
        assert!(!json.contains("cat"), "arguments leaked: {json}");
    }

    /// A hallucinated or MCP tool name is model-chosen text, so it collapses to
    /// the closed vocabulary rather than echoing.
    #[test]
    fn an_unknown_tool_name_collapses_to_other() {
        let (mut bridge, records) = bridge();
        bridge.observe(&tool_call("exfiltrate_ssh_keys_now", serde_json::json!({})));
        let json = serde_json::to_string(&records.records()[0]).expect("serialize");
        assert!(json.contains(r#""tool":"other""#), "{json}");
        assert!(!json.contains("exfiltrate"), "{json}");
    }

    /// The failure signal a benchmark actually needs: a tool failed, how long
    /// it took, and how much it said — without what it said.
    #[test]
    fn a_failed_tool_result_is_a_warning_carrying_no_message() {
        let (mut bridge, records) = bridge();
        bridge.observe(&AgentEvent::ToolResult {
            call_id: "call-1".into(),
            output: ToolOutput::Error {
                message: "/home/ada/secret.rs:12: permission denied".into(),
            },
            duration_ms: 1234,
            speculated: false,
        });

        let record = records.find("agent.tool.result").expect("a record");
        assert_eq!(record.level, Level::Warn);
        let json = serde_json::to_string(&record).expect("serialize");
        assert!(json.contains(r#""ok":false"#), "{json}");
        assert!(json.contains(r#""duration_ms":1234"#), "{json}");
        assert!(!json.contains("ada"), "the message leaked: {json}");
        assert!(!json.contains("permission denied"), "{json}");
    }

    /// A manifest advances the correlation context, so everything recorded
    /// after it is attributable to that turn and step without re-threading.
    #[test]
    fn a_manifest_advances_the_correlation_context() {
        let (mut bridge, records) = bridge();
        bridge.observe(&AgentEvent::StepManifest {
            turn_instance: 3,
            step: 7,
            call_seq: 0,
            role: ModelCallRole::Worker,
            provider: "anthropic".into(),
            model: "claude-fable-5".into(),
            blocks: Vec::new(),
            effective_budget_tokens: 100_000,
            calibration_factor: 1.0,
            estimated_input_tokens: 4_200,
            compiled_frame: None,
        });
        assert_eq!(bridge.cx.turn, Some(3));
        assert_eq!(bridge.cx.step, Some(7));

        bridge.observe(&tool_call("grep", serde_json::json!({})));
        let record = records.find("agent.tool.call").expect("a record");
        assert_eq!(record.cx.turn, Some(3), "the tool call inherits the turn");
        assert_eq!(record.cx.step, Some(7));
    }

    /// Compaction is fourteen numbers and no strings — the single richest
    /// "why did this run behave that way" record available, and it needs no
    /// new emit site in stella-core.
    #[test]
    fn compaction_records_every_number_it_was_given() {
        let (mut bridge, records) = bridge();
        bridge.observe(&AgentEvent::Compaction {
            before_tokens: 120_000,
            after_tokens: 60_000,
            evicted: 4,
            deduped: 2,
            superseded: 1,
            aged: 3,
            summarized: 5,
            evicted_blocks: vec!["blk-secret-name".into()],
            deduped_blocks: Vec::new(),
            superseded_blocks: Vec::new(),
            aged_blocks: Vec::new(),
            summarized_blocks: Vec::new(),
            effective_budget_tokens: 80_000,
            calibration_factor: 1.25,
        });

        let record = records.find("agent.context.compaction").expect("a record");
        let json = serde_json::to_string(&record).expect("serialize");
        assert!(json.contains(r#""before_tokens":120000"#), "{json}");
        assert!(json.contains(r#""after_tokens":60000"#), "{json}");
        assert!(json.contains(r#""summarized":5"#), "{json}");
        assert!(
            !json.contains("blk-secret-name"),
            "block ids are content-adjacent and must not ride along: {json}"
        );
    }

    /// A path is classified, never spelled.
    #[test]
    fn a_file_change_records_a_path_class_not_a_path() {
        let (mut bridge, records) = bridge();
        bridge.observe(&AgentEvent::FileChange {
            path: "/w/crates/secretproject/src/main.rs".into(),
            kind: FileChangeKind::Modified,
            added: 12,
            removed: 3,
            diff: Some("- old secret\n+ new secret".into()),
        });

        let json = serde_json::to_string(&records.records()[0]).expect("serialize");
        assert!(json.contains(r#""class":"inside_workspace""#), "{json}");
        assert!(json.contains(r#""ext":"rs""#), "{json}");
        assert!(json.contains(r#""added":12"#), "{json}");
        assert!(!json.contains("secretproject"), "{json}");
        assert!(!json.contains("new secret"), "the diff leaked: {json}");
    }

    /// The provider and model are the one runtime string a benchmark log is
    /// useless without, so they come through the reviewed hatch — carrying the
    /// justification with them.
    #[test]
    fn step_usage_carries_the_model_through_the_reviewed_hatch() {
        let (mut bridge, records) = bridge();
        bridge.observe(&AgentEvent::StepUsage {
            step: 1,
            role: ModelCallRole::Worker,
            provider: "anthropic".into(),
            model: "claude-fable-5".into(),
            output_text: Some("the model's actual answer".into()),
            input_tokens: 1000,
            output_tokens: 200,
            cached_input_tokens: 900,
            cache_write_tokens: 0,
            estimated_input_tokens: 1010,
            cost_usd: 0.0123,
            duration_ms: 4200,
            retries: 2,
            tool_calls: 1,
            complete: true,
            finish_reason: None,
        });

        let json = serde_json::to_string(&records.records()[0]).expect("serialize");
        assert!(json.contains("claude-fable-5"), "{json}");
        assert!(json.contains("chosen by the operator"), "{json}");
        assert!(json.contains(r#""duration_ms":4200"#), "{json}");
        assert!(json.contains(r#""retries":2"#), "{json}");
        assert!(
            !json.contains("actual answer"),
            "output_text leaked: {json}"
        );
    }

    /// Every level assignment that a benchmark post-mortem greps for.
    #[test]
    fn failure_shaped_events_are_recorded_loudly() {
        let (mut bridge, records) = bridge();
        bridge.observe(&AgentEvent::RetriesExhausted {
            turn_instance: 1,
            attempts: 4,
            reasons: vec!["529 overloaded".into()],
            retryable: true,
        });
        bridge.observe(&AgentEvent::BudgetDenied {
            scope: BudgetScope::Session,
            spent_usd: 5.5,
            limit_usd: 5.0,
            mode: stella_protocol::BudgetMode::Enforced,
        });
        bridge.observe(&AgentEvent::Error {
            message: "the provider said something with a path in it".into(),
            retryable: false,
        });

        for (code, level) in [
            ("agent.model.retries_exhausted", Level::Error),
            ("agent.budget.denied", Level::Error),
            ("agent.error", Level::Error),
        ] {
            assert_eq!(records.find(code).expect(code).level, level, "{code}");
        }
        let json = serde_json::to_string(&records.records()).expect("serialize");
        assert!(!json.contains("529 overloaded"), "{json}");
        assert!(!json.contains("path in it"), "{json}");
    }

    /// The bridge's ordinal counts every event, including the ones that emit
    /// nothing — that is what makes it the raw stream position rather than a
    /// record index.
    #[test]
    fn the_sequence_counts_every_event_not_every_record() {
        let (mut bridge, records) = bridge();
        for _ in 0..5 {
            bridge.observe(&AgentEvent::TextDelta { text: "x".into() });
        }
        bridge.observe(&tool_call("bash", serde_json::json!({})));

        let record = records.find("agent.tool.call").expect("a record");
        assert_eq!(
            record.fields.get("seq"),
            Some(&stella_diag::FieldValue::Uint(6)),
            "the five silent deltas still advanced the ordinal"
        );
    }
}
