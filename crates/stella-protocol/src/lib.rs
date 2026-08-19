// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella-protocol` — serde types shared by every crate in the `stella-cli`
//! workspace: agent events, tool schemas, trace records, and provider
//! request/response envelopes.
//!
//! Zero logic, zero I/O. This is the stability contract of the whole
//! workspace — any type here that crosses a process/protocol boundary must
//! round-trip through `serde_json` byte-for-byte (see the round-trip tests in
//! each module).
//!
//! Wire compatibility is additive, and it now holds in **both** directions.
//! New *fields* ride `serde(default)`, so a newer binary parses every older
//! stream. New [`AgentEvent`] *variants* travel backwards via
//! [`AgentEvent::Unknown`]: an older binary meets an unrecognized `"type"` by
//! preserving the event whole and moving on, instead of failing the stream.
//!
//! What travels backwards is the *variant*, not the field. An unrecognized key
//! on a tag this build already knows parses (serde ignores it) and is then
//! dropped on re-serialization — so relaying a newer stream keeps its new
//! events whole while quietly narrowing its new fields. See the `event`
//! module docs.
//!
//! The tolerance is scoped to the tag alone. A `"type"` this build *does*
//! know, carrying a body that does not fit its variant, is still a hard
//! error — that is corruption or an encoder bug, not a version skew, and it
//! must not be laundered into [`AgentEvent::Unknown`]. [`KNOWN_TYPE_TAGS`] is
//! the exact boundary between the two cases.
//!
//! Read "tag alone" literally: the *nested* enums a variant carries
//! ([`ModelCallRole`], [`StageKind`], [`PolicyKind`], [`CiStatus`], and
//! their peers) are closed, so a value from a newer vocabulary lands on the
//! hard-error side and costs the reader the whole event. [`BlockKind`] and
//! [`CacheZone`] are the two exceptions, and even they degrade lossily —
//! the future token is not preserved across a round trip. Growing one of
//! those vocabularies is still a one-directional change.
//!
//! [`context_event::LifecycleEventEnvelope`] remains the right home for
//! internal lifecycle events: it adds an explicit `schema_version` and keeps
//! stella-internal vocabulary off the public event stream. It is no longer
//! the *only* way to stay readable by an older binary.

#![forbid(unsafe_code)]
// AGENTS.md rule #5: library code never panics on runtime data. This crate's
// default build has zero production `unwrap`/`expect`, and `make lint` runs
// clippy with `-D warnings` on that build, so a new one fails the gate instead
// of arriving unremarked. The one exception is `schema_export.rs`, which is
// only compiled under the `schema` feature (not part of `make lint` or CI's
// clippy run) and carries its own justified `#[allow(clippy::expect_used)]`.
// `not(test)` scopes the lint exactly as the rule does: it is about runtime
// data, and `unwrap` in a test is fine.
#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used))]

pub mod attachment;
pub mod cache;
pub mod candidate;
pub mod compaction_rewrite;
pub mod completion;
pub mod context_event;
pub mod contract;
pub mod delivery_event;
pub mod denial;
pub mod error;
pub mod event;
pub mod hook;
pub mod journal;
pub mod ladder;
pub mod lane;
pub mod proof;
pub mod provider;
pub mod recall;
pub mod receipt;
pub mod role;
#[cfg(feature = "schema")]
pub mod schema_export;
pub mod subagent_event;
pub mod tokens;
pub mod tool;

pub use attachment::{
    Attachment, AttachmentKind, AttachmentSource, classify_media_type, human_bytes,
    media_type_for_path,
};
pub use cache::CacheCause;
// A candidate worktree, named so it can cross a process (#3380 A10). Here
// rather than in `stella-plugin` because a handle is a value the *host* mints
// at run time, not a manifest declaration parsed out of borrowed text — see
// the module docs for the full argument.
pub use candidate::{CandidateDenial, CandidateHandle, CandidateOp, PathDenial};
pub use compaction_rewrite::CompactionRewrite;
pub use completion::{
    CompletionMessage, CompletionRequest, CompletionRequestRef, CompletionResult, CompletionUsage,
    FinishReason, GenerationParams, MessageRole, PartialUsage, ReasoningEffort, ServiceTier,
    Verbosity,
};
pub use context_event::{CompiledContextFrameBuilt, LifecycleEvent, LifecycleEventEnvelope};
pub use contract::{ContractError, Provenance, RiskLevel, ToolContract};
pub use delivery_event::{DeliveryDecline, DeliveryOutcome};
// The payload of a gate's refusal (#3380). Lives here, not beside the decision
// enum in `stella-core::bus`, because the trace fold and a future out-of-process
// host both read it and that file is closed to growth.
pub use denial::{Denial, DenialEvidence};
pub use error::ProviderError;
pub use event::{
    AgentEvent, BlockKind, BlockOrigin, BudgetMode, BudgetScope, CacheZone, CiStatus,
    ContextFrameRef, ContextProviderUsage, ContextUsage, FileChangeKind, HunkProposal,
    KNOWN_TYPE_TAGS, ManifestEntry, MediaArtifactRef, MediaJobState, MediaKind, ModelCallRole,
    PolicyKind, PrStatus, ProofStep, ProposedHunk, ProviderShare, ScopeProposal, StageKind,
    StageScope, TaskItem, TaskStatus, UNKNOWN_MODEL, UsageIncompleteReason, VerdictEvidence,
};
// The journal line is the event plus the wall-clock stamp its sink adds
// (#2111). Deliberately a separate type from `AgentEvent`: a stamp is a fact
// about a write, and the engine that produces events owns no clock.
// The lifecycle-hook vocabulary lives here so `stella-core` (which dispatches
// the hooks) and `stella-plugin` (which grants them) can share one enum
// without either depending on the other (#3310).
pub use hook::HookEvent;
pub use journal::{StampedEvent, stamped_line};
pub use lane::{BuiltinLane, LaneId, TurnLane};
// The ladder vocabulary moved out of `event` when the rung joined it (#1043);
// re-exported here so `stella_protocol::LadderSnapshot` never moved.
pub use ladder::{FlipOutcome, LadderRung, LadderSnapshot, OracleObservation, ProofTree};
pub use provider::{Provider, ToolCallObserver};
pub use role::{ModelRef, Role};
pub use subagent_event::{SubAgentPhase, SubAgentStatus};
pub use tokens::{
    BYTES_PER_TOKEN, estimate_token_cost, estimate_tokens, estimate_tokens_for_bytes,
};
pub use tool::{ErrorClass, ToolCall, ToolOutput, ToolResult, ToolSchema};
