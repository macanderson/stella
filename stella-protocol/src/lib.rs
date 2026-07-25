//! `stella-protocol` — the serde vocabulary every crate in the `stella-cli`
//! workspace shares: agent events, tool schemas, multimodal attachments,
//! model roles, prompt-cache diagnoses, adaptive-context lifecycle events,
//! provider request/response envelopes, and the [`Provider`] port itself.
//!
//! Zero logic, zero I/O. This is the stability contract of the whole
//! workspace — any type here that crosses a process/protocol boundary must
//! round-trip through `serde_json` byte-for-byte (see the round-trip tests in
//! each module).
//!
//! Wire compatibility is additive, but in exactly **one** direction. New
//! *fields* ride `serde(default)`, so a newer binary parses every older
//! stream. New [`AgentEvent`] *variants* do not travel backwards: the enum is
//! internally tagged, so an older binary meets an unrecognized `"type"` with a
//! hard deserialization error. An event that must survive being read by an
//! older binary rides the versioned
//! [`context_event::LifecycleEventEnvelope`] instead, whose payload stays raw.

#![forbid(unsafe_code)]

pub mod attachment;
pub mod cache;
pub mod completion;
pub mod context_event;
pub mod error;
pub mod event;
pub mod provider;
pub mod role;
pub mod tool;

pub use attachment::{
    Attachment, AttachmentKind, AttachmentSource, classify_media_type, human_bytes,
    media_type_for_path,
};
pub use cache::CacheCause;
pub use completion::{
    CompletionMessage, CompletionRequest, CompletionResult, CompletionUsage, FinishReason,
    GenerationParams, MessageRole, ReasoningEffort, ServiceTier, Verbosity,
};
pub use error::ProviderError;
pub use event::{
    AgentEvent, BlockKind, BlockOrigin, BudgetMode, BudgetScope, CacheZone, CiStatus,
    ContextFrameRef, ContextProviderUsage, ContextUsage, FileChangeKind, JudgeEvidence,
    ManifestEntry, MediaArtifactRef, MediaJobState, MediaKind, ModelCallRole, PolicyKind, PrStatus,
    ProviderShare, ScopeProposal, StageKind, TaskItem, TaskStatus, UsageIncompleteReason,
};
pub use provider::{Provider, ToolCallObserver};
pub use role::{ModelRef, Role};
pub use tool::{ToolCall, ToolOutput, ToolResult, ToolSchema};
