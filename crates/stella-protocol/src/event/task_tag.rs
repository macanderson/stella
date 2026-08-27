// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Which [`AgentEvent`] cases carry a [`TaskId`], and the three total
//! accessors over that fact.
//!
//! A task's evidence ledger is a *selection* (`design/tui-v2/SPEC.md` §7.1),
//! so something has to answer "does this event name a task, and which one?"
//! for an arbitrary case. Every consumer that asks — the engine stamping a
//! dispatch, the store projecting a row, a surface rendering `→ task 3` —
//! would otherwise write its own `match`, and each copy would silently answer
//! `None` for a case added after it was written.
//!
//! Do not add a wildcard arm to the table below. It lists the carrying and the
//! non-carrying cases separately so both `match`es are exhaustive, which makes
//! the classification `E0004`; a `_ => None` arm would let a new case fall
//! through to *untagged* and be wrong only at runtime, as a task whose ledger
//! misses a whole class of its own work. That is the shape `event/tags.rs`
//! warns about under "Silent — wildcard arms the compiler CANNOT catch".

use super::AgentEvent;
use crate::task_id::TaskId;

/// Expands the carrier table into the accessors over it.
///
/// # What is in the carrying half, and why the rest is not
///
/// SPEC 7.1 defines the ledger as "the ledger of events tagged with this task
/// id (edits, runs, graph writes)" and the cost line as `$ · tok · cache rd% ·
/// model calls · est remain`. The six carriers are what those two sentences
/// need:
///
/// - [`AgentEvent::ToolStart`] / [`AgentEvent::ToolResult`] — the runs, and
///   the reads, edits, writes and deletes that ride the same pair.
/// - [`AgentEvent::FileChange`] — the edits, as measured rather than declared.
/// - [`AgentEvent::ContextWrite`] — the graph and memory writes.
/// - [`AgentEvent::StepUsage`] — the metering record, and so the whole cost
///   line except the estimate.
/// - [`AgentEvent::UsageIncomplete`] — its sibling, and what lets a per-task
///   total state its own confidence: a paid call that died without a usage
///   envelope is spend the fold cannot see, so a task carrying one of these
///   has a cost that is a **lower bound** and can say so (#4147 makes the same
///   distinction one scope up).
///
/// Everything else is untagged, for three reasons rather than one:
///
/// - **Turn- and run-scoped facts** ([`AgentEvent::Stage`],
///   [`AgentEvent::TurnComplete`], [`AgentEvent::RunComplete`],
///   [`AgentEvent::BudgetTick`], [`AgentEvent::Compaction`],
///   [`AgentEvent::SteeringWithheld`]) belong to a span that can *contain*
///   several tasks. Tagging one with whichever task happened to be running
///   would make a turn's total look like a task's.
/// - **Narration** ([`AgentEvent::Text`], [`AgentEvent::TextDelta`],
///   [`AgentEvent::Reasoning`]) is not work, and SPEC 6.2 renders the tag on
///   an event's head — prose has no head to render it on. The board's own
///   traffic ([`AgentEvent::TaskUpdate`]) is the mirror case: it carries every
///   task at once, so naming one of them would be a category error.
/// - **Delivery and verification** ([`AgentEvent::Commit`],
///   [`AgentEvent::Pr`], [`AgentEvent::Proof`], [`AgentEvent::Verdict`]) are
///   claims about the *run's* output. A task's own claim is its
///   [`TaskContract`](crate::TaskContract), which the board already carries,
///   and a second per-task verification channel here would rival it.
///
/// None of that is closed: moving a case across is one line in the table plus
/// its field, and the compiler keeps the table total either way.
macro_rules! task_tagged_events {
    (carries { $($carry:ident),* $(,)? } untagged { $($plain:ident),* $(,)? }) => {
        impl AgentEvent {
            /// The board task this event belongs to, or `None` — either
            /// because the case carries no tag at all (see the module doc for
            /// the split) or because nothing stamped one.
            ///
            /// A consumer that needs to tell those two apart asks
            /// [`AgentEvent::carries_task_tag`]; a consumer building a
            /// ledger does not, because both answers mean "not this task's".
            #[must_use]
            pub fn task_id(&self) -> Option<&TaskId> {
                match self {
                    $(AgentEvent::$carry { task_id, .. } => task_id.as_ref(),)*
                    $(AgentEvent::$plain { .. } => None,)*
                }
            }

            /// Whether this case has a slot for a tag at all, however it is
            /// currently filled.
            ///
            /// The question a stamping producer asks before it pays for a
            /// board read: most of a turn's stream is narration, and looking
            /// up the running task for an event that could not carry it is
            /// pure cost.
            #[must_use]
            pub fn carries_task_tag(&self) -> bool {
                match self {
                    $(AgentEvent::$carry { .. } => true,)*
                    $(AgentEvent::$plain { .. } => false,)*
                }
            }

            /// Fill an **empty** tag slot, reporting whether this event now
            /// names `task`.
            ///
            /// Never overwrites: a tag already present was set by a producer
            /// closer to the work — a sub-agent lane that knows its own task
            /// — and the ambient running task must not relabel it. That is
            /// the whole of the stamping contract's third clause
            /// ([`TaskId`]'s module doc), expressed where it cannot be
            /// forgotten, rather than as a rule each producer re-implements.
            pub fn stamp_task(&mut self, task: &TaskId) -> bool {
                match self {
                    $(AgentEvent::$carry { task_id, .. } => {
                        if task_id.is_none() {
                            *task_id = Some(task.clone());
                        }
                        true
                    })*
                    $(AgentEvent::$plain { .. } => false,)*
                }
            }
        }
    };
}

task_tagged_events! {
    carries {
        ToolStart,
        ToolResult,
        FileChange,
        ContextWrite,
        StepUsage,
        UsageIncomplete,
    }
    untagged {
        SkillInjected,
        Stage,
        Text,
        TextDelta,
        Reasoning,
        SpeculationDiscarded,
        Retry,
        Steered,
        TurnParked,
        TurnWoken,
        LoopDetected,
        BudgetDenied,
        RetriesExhausted,
        PolicyDecision,
        Compaction,
        BudgetTick,
        GoalVerdict,
        ProviderFallback,
        ContextRecall,
        BlockRegistered,
        StepManifest,
        Proof,
        Verdict,
        ScopeReview,
        HunkReview,
        AskUser,
        MediaProgress,
        MediaComplete,
        Commit,
        Pr,
        TaskUpdate,
        SubAgent,
        CandidateDelivery,
        Error,
        TurnComplete,
        RunComplete,
        SteeringWithheld,
        Unknown,
    }
}
