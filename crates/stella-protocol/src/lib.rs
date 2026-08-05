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

pub mod attachment;
pub mod cache;
pub mod completion;
pub mod context_event;
pub mod error;
pub mod event;
pub mod ladder;
pub mod provider;
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
pub use completion::{
    CompletionMessage, CompletionRequest, CompletionRequestRef, CompletionResult, CompletionUsage,
    FinishReason, GenerationParams, MessageRole, PartialUsage, ReasoningEffort, ServiceTier,
    Verbosity,
};
pub use context_event::{CompiledContextFrameBuilt, LifecycleEvent, LifecycleEventEnvelope};
pub use error::ProviderError;
pub use event::{
    AgentEvent, BlockKind, BlockOrigin, BudgetMode, BudgetScope, CacheZone, CiStatus,
    ContextFrameRef, ContextProviderUsage, ContextUsage, FileChangeKind, HunkProposal,
    KNOWN_TYPE_TAGS, ManifestEntry, MediaArtifactRef, MediaJobState, MediaKind, ModelCallRole,
    PolicyKind, PrStatus, ProofStep, ProposedHunk, ProviderShare, ScopeProposal, StageKind,
    TaskItem, TaskStatus, UsageIncompleteReason, VerdictEvidence,
};
// The ladder vocabulary moved out of `event` when the rung joined it (#1043);
// re-exported here so `stella_protocol::LadderSnapshot` never moved.
pub use ladder::{LadderRung, LadderSnapshot, OracleObservation, ProofTree};
pub use provider::{Provider, ToolCallObserver};
pub use role::{ModelRef, Role};
pub use subagent_event::{SubAgentPhase, SubAgentStatus};
pub use tokens::{
    BYTES_PER_TOKEN, estimate_token_cost, estimate_tokens, estimate_tokens_for_bytes,
};
pub use tool::{ToolCall, ToolOutput, ToolResult, ToolSchema};
