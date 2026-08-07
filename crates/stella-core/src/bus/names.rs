//! The event-name catalog. Names are dotted, lowercase, and namespaced;
//! subscriptions match exactly, by namespace wildcard (`file.*`, `tool.*`,
//! `tool.call.*`), or globally (`*`). Extensions may emit custom names —
//! the catalog is the contract for what the *host* emits, not a closed set.

// ---- session ----
pub const SESSION_CREATED: &str = "session.created";
pub const SESSION_STARTED: &str = "session.started";
pub const SESSION_PAUSED: &str = "session.paused";
pub const SESSION_RESUMED: &str = "session.resumed";
pub const SESSION_CANCEL_REQUESTED: &str = "session.cancel_requested";
pub const SESSION_CANCELLED: &str = "session.cancelled";
pub const SESSION_COMPLETED: &str = "session.completed";
pub const SESSION_FAILED: &str = "session.failed";
// ---- agent + transcript ----
pub const AGENT_TURN_STARTED: &str = "agent.turn.started";
pub const AGENT_THINKING_STARTED: &str = "agent.thinking.started";
pub const AGENT_THINKING_DELTA: &str = "agent.thinking.delta";
pub const AGENT_THINKING_COMPLETED: &str = "agent.thinking.completed";
pub const AGENT_MESSAGE_CREATED: &str = "agent.message.created";
pub const AGENT_MESSAGE_DELTA: &str = "agent.message.delta";
pub const AGENT_MESSAGE_COMPLETED: &str = "agent.message.completed";
pub const AGENT_TURN_COMPLETED: &str = "agent.turn.completed";
/// One committed engine step began. The unit between `agent.turn.*` and
/// `model.request.*`: a turn is a sequence of steps, and a step is at most
/// one model call plus the tool dispatch that answers it.
///
/// Added with the emitters (#1133) rather than pre-declared, because the
/// catalog is the contract for what the host emits and a name nothing
/// emits is a promise, not a contract.
pub const AGENT_STEP_STARTED: &str = "agent.step.started";
/// One engine step ended, carrying whether the turn continues. Emitted on
/// every exit from a step — including the abort paths — so an observer
/// counting starts against completions never leaks.
pub const AGENT_STEP_COMPLETED: &str = "agent.step.completed";
/// The turn parked on an engine-side wait (#1471, #1857): probes run on the
/// engine's clock and no model call happens until the wake. Observer-only —
/// deliberately NOT in [`BLOCKING`]: the park replays only read-only calls,
/// so there is nothing policy-sensitive to intercept.
pub const AGENT_TURN_PARKED: &str = "agent.turn.parked";
/// The parked turn woke — closes the span `agent.turn.parked` opened.
pub const AGENT_TURN_WOKEN: &str = "agent.turn.woken";
pub const AGENT_ERROR: &str = "agent.error";
pub const TRANSCRIPT_ENTRY_CREATED: &str = "transcript.entry.created";
pub const TRANSCRIPT_ENTRY_UPDATED: &str = "transcript.entry.updated";
// ---- model ----
pub const MODEL_REQUEST_STARTED: &str = "model.request.started";
pub const MODEL_REQUEST_COMPLETED: &str = "model.request.completed";
pub const MODEL_REQUEST_FAILED: &str = "model.request.failed";
pub const MODEL_RESPONSE_STARTED: &str = "model.response.started";
pub const MODEL_RESPONSE_DELTA: &str = "model.response.delta";
pub const MODEL_RESPONSE_COMPLETED: &str = "model.response.completed";
pub const MODEL_RATE_LIMITED: &str = "model.rate_limited";
pub const MODEL_CONTEXT_COMPACTED: &str = "model.context.compacted";
// ---- tools ----
pub const TOOL_REGISTERED: &str = "tool.registered";
pub const TOOL_CALL_REQUESTED: &str = "tool.call.requested";
pub const TOOL_CALL_VALIDATED: &str = "tool.call.validated";
pub const TOOL_CALL_STARTED: &str = "tool.call.started";
pub const TOOL_CALL_PROGRESS: &str = "tool.call.progress";
pub const TOOL_CALL_COMPLETED: &str = "tool.call.completed";
pub const TOOL_CALL_FAILED: &str = "tool.call.failed";
pub const TOOL_CALL_CANCELLED: &str = "tool.call.cancelled";
// ---- policy + approval ----
pub const POLICY_EVALUATED: &str = "policy.evaluated";
pub const POLICY_ALLOWED: &str = "policy.allowed";
pub const POLICY_BLOCKED: &str = "policy.blocked";
pub const APPROVAL_REQUESTED: &str = "approval.requested";
pub const APPROVAL_GRANTED: &str = "approval.granted";
pub const APPROVAL_DENIED: &str = "approval.denied";
pub const APPROVAL_EXPIRED: &str = "approval.expired";
pub const SECRET_DETECTED: &str = "secret.detected";
pub const SENSITIVE_OPERATION_DETECTED: &str = "sensitive_operation.detected";
// ---- files + workspace ----
pub const FILE_READ: &str = "file.read";
pub const FILE_CREATED: &str = "file.created";
pub const FILE_UPDATED: &str = "file.updated";
pub const FILE_DELETED: &str = "file.deleted";
pub const FILE_RENAMED: &str = "file.renamed";
pub const FILE_DIFF_COMPUTED: &str = "file.diff.computed";
pub const FILES_TOUCHED_UPDATED: &str = "files_touched.updated";
pub const WORKSPACE_OPENED: &str = "workspace.opened";
pub const WORKSPACE_INDEX_STARTED: &str = "workspace.index.started";
pub const WORKSPACE_INDEX_COMPLETED: &str = "workspace.index.completed";
pub const SEARCH_STARTED: &str = "search.started";
pub const SEARCH_COMPLETED: &str = "search.completed";
pub const SEARCH_FAILED: &str = "search.failed";
// ---- commands + validation ----
pub const COMMAND_STARTED: &str = "command.started";
pub const COMMAND_STDOUT: &str = "command.stdout";
pub const COMMAND_STDERR: &str = "command.stderr";
pub const COMMAND_COMPLETED: &str = "command.completed";
pub const COMMAND_FAILED: &str = "command.failed";
pub const BUILD_STARTED: &str = "build.started";
pub const BUILD_COMPLETED: &str = "build.completed";
pub const BUILD_FAILED: &str = "build.failed";
pub const TEST_STARTED: &str = "test.started";
pub const TEST_COMPLETED: &str = "test.completed";
pub const TEST_FAILED: &str = "test.failed";
pub const DIAGNOSTIC_DETECTED: &str = "diagnostic.detected";
pub const DIAGNOSTIC_RESOLVED: &str = "diagnostic.resolved";
// ---- git + delivery ----
pub const GIT_STATUS_CHANGED: &str = "git.status.changed";
pub const GIT_DIFF_CREATED: &str = "git.diff.created";
pub const GIT_COMMIT_REQUESTED: &str = "git.commit.requested";
pub const GIT_COMMIT_CREATED: &str = "git.commit.created";
pub const GIT_PUSH_REQUESTED: &str = "git.push.requested";
pub const GIT_PUSH_COMPLETED: &str = "git.push.completed";
pub const PULL_REQUEST_REQUESTED: &str = "pull_request.requested";
pub const PULL_REQUEST_CREATED: &str = "pull_request.created";
pub const DEPLOYMENT_REQUESTED: &str = "deployment.requested";
pub const DEPLOYMENT_COMPLETED: &str = "deployment.completed";
pub const DEPLOYMENT_FAILED: &str = "deployment.failed";
// ---- extensions + telemetry ----
pub const EXTENSION_LOADED: &str = "extension.loaded";
pub const EXTENSION_UNLOADED: &str = "extension.unloaded";
pub const EXTENSION_ERROR: &str = "extension.error";
/// An observer was quarantined (skipped for the rest of the session) after
/// repeatedly overrunning its per-dispatch time budget — a resilience
/// signal so a wedged extension can't silently stall the emitting thread.
pub const EXTENSION_QUARANTINED: &str = "extension.quarantined";
pub const TELEMETRY_EVENT_QUEUED: &str = "telemetry.event.queued";
pub const TELEMETRY_EVENT_FLUSHED: &str = "telemetry.event.flushed";
pub const TELEMETRY_EVENT_FAILED: &str = "telemetry.event.failed";

/// Every catalog name, grouped as above. Extensions may emit names not
/// in this list; hosts should not.
pub const ALL: &[&str] = &[
    SESSION_CREATED,
    SESSION_STARTED,
    SESSION_PAUSED,
    SESSION_RESUMED,
    SESSION_CANCEL_REQUESTED,
    SESSION_CANCELLED,
    SESSION_COMPLETED,
    SESSION_FAILED,
    AGENT_TURN_STARTED,
    AGENT_THINKING_STARTED,
    AGENT_THINKING_DELTA,
    AGENT_THINKING_COMPLETED,
    AGENT_MESSAGE_CREATED,
    AGENT_MESSAGE_DELTA,
    AGENT_MESSAGE_COMPLETED,
    AGENT_STEP_STARTED,
    AGENT_STEP_COMPLETED,
    AGENT_TURN_PARKED,
    AGENT_TURN_WOKEN,
    AGENT_TURN_COMPLETED,
    AGENT_ERROR,
    TRANSCRIPT_ENTRY_CREATED,
    TRANSCRIPT_ENTRY_UPDATED,
    MODEL_REQUEST_STARTED,
    MODEL_REQUEST_COMPLETED,
    MODEL_REQUEST_FAILED,
    MODEL_RESPONSE_STARTED,
    MODEL_RESPONSE_DELTA,
    MODEL_RESPONSE_COMPLETED,
    MODEL_RATE_LIMITED,
    MODEL_CONTEXT_COMPACTED,
    TOOL_REGISTERED,
    TOOL_CALL_REQUESTED,
    TOOL_CALL_VALIDATED,
    TOOL_CALL_STARTED,
    TOOL_CALL_PROGRESS,
    TOOL_CALL_COMPLETED,
    TOOL_CALL_FAILED,
    TOOL_CALL_CANCELLED,
    POLICY_EVALUATED,
    POLICY_ALLOWED,
    POLICY_BLOCKED,
    APPROVAL_REQUESTED,
    APPROVAL_GRANTED,
    APPROVAL_DENIED,
    APPROVAL_EXPIRED,
    SECRET_DETECTED,
    SENSITIVE_OPERATION_DETECTED,
    FILE_READ,
    FILE_CREATED,
    FILE_UPDATED,
    FILE_DELETED,
    FILE_RENAMED,
    FILE_DIFF_COMPUTED,
    FILES_TOUCHED_UPDATED,
    WORKSPACE_OPENED,
    WORKSPACE_INDEX_STARTED,
    WORKSPACE_INDEX_COMPLETED,
    SEARCH_STARTED,
    SEARCH_COMPLETED,
    SEARCH_FAILED,
    COMMAND_STARTED,
    COMMAND_STDOUT,
    COMMAND_STDERR,
    COMMAND_COMPLETED,
    COMMAND_FAILED,
    BUILD_STARTED,
    BUILD_COMPLETED,
    BUILD_FAILED,
    TEST_STARTED,
    TEST_COMPLETED,
    TEST_FAILED,
    DIAGNOSTIC_DETECTED,
    DIAGNOSTIC_RESOLVED,
    GIT_STATUS_CHANGED,
    GIT_DIFF_CREATED,
    GIT_COMMIT_REQUESTED,
    GIT_COMMIT_CREATED,
    GIT_PUSH_REQUESTED,
    GIT_PUSH_COMPLETED,
    PULL_REQUEST_REQUESTED,
    PULL_REQUEST_CREATED,
    DEPLOYMENT_REQUESTED,
    DEPLOYMENT_COMPLETED,
    DEPLOYMENT_FAILED,
    EXTENSION_LOADED,
    EXTENSION_UNLOADED,
    EXTENSION_ERROR,
    EXTENSION_QUARANTINED,
    TELEMETRY_EVENT_QUEUED,
    TELEMETRY_EVENT_FLUSHED,
    TELEMETRY_EVENT_FAILED,
];

/// The events hosts route through [`super::HookBus::emit_blocking`] —
/// the explicit allowlist of interceptable, policy-sensitive actions.
pub const BLOCKING: &[&str] = &[
    TOOL_CALL_REQUESTED,
    FILE_CREATED,
    FILE_UPDATED,
    FILE_DELETED,
    COMMAND_STARTED,
    GIT_COMMIT_REQUESTED,
    GIT_PUSH_REQUESTED,
    PULL_REQUEST_REQUESTED,
    DEPLOYMENT_REQUESTED,
];

/// Whether `name` is in the host catalog.
pub fn is_known(name: &str) -> bool {
    ALL.contains(&name)
}

/// Whether `name` is a blocking (interceptable) event.
pub fn is_blocking(name: &str) -> bool {
    BLOCKING.contains(&name)
}
