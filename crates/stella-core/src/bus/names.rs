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
/// The overflow summarizer's splice was followed by a working-set
/// restoration (#2685): recently-read files re-read fresh and/or an active
/// skill body re-attached as one tail message. Observer-only, counts only —
/// deliberately NOT in [`BLOCKING`]: the restoration replays only read-only
/// calls, so there is nothing policy-sensitive to intercept (the same
/// posture as `agent.turn.parked`).
pub const AGENT_WORKING_SET_RESTORED: &str = "agent.working_set.restored";
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
// ---- settings-declared user hooks (#2684) ----
/// A `Stop` hook held a completing turn open; the payload carries the
/// hook's reason, which also reaches the model as a marked tail message
/// (`driver::user_hooks`).
pub const HOOK_STOP_BLOCKED: &str = "hook.stop.blocked";
/// A `PreCompact` hook vetoed an overflow-summarization round; the payload
/// carries the hook's reason.
pub const HOOK_PRE_COMPACT_VETOED: &str = "hook.pre_compact.vetoed";
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
    AGENT_WORKING_SET_RESTORED,
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
    HOOK_STOP_BLOCKED,
    HOOK_PRE_COMPACT_VETOED,
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

// ---- plugin namespace (A9, docs/spec/pipeline-as-plugins.md §4) ----
//
// `plugin.<id>.*` was not contemplated when this module's opening comment
// was written — the string "plugin" appears nowhere above this line. What
// the opening comment's "extensions may emit custom names" already granted
// is the *permission*; this section is what makes that permission a
// reserved, collision-free namespace instead of a convention a plugin could
// accidentally (or adversarially) step outside of. Every name here lands in
// the journal as [`stella_protocol::AgentEvent::Unknown`] — see
// `stella-cli/src/trace.rs`'s fold arm — never as a new `AgentEvent`
// variant, so it carries no row in the signal-consumer ledger
// (`stella-protocol/src/event/consumers.rs`) and needs none: `Unknown` is
// the vocabulary's designed extension point, not a gap in it.

/// Prefix reserving the whole namespace for plugin-authored event names. No
/// name in [`ALL`] may start with it (`plugin_names_never_collide_with_the_host_catalog`
/// below enforces that on every host name this build declares), and the only
/// sanctioned way to build a name that *does* start with it is
/// [`plugin_event_name`], which validates the id and local segment first.
pub const PLUGIN_NAMESPACE_PREFIX: &str = "plugin.";

/// Why a candidate plugin id, or the local segment of a plugin event name,
/// was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PluginNamespaceError {
    /// A plugin id must name something.
    #[error("plugin id must not be empty")]
    EmptyId,
    /// Ids are restricted to `[a-z0-9_-]` specifically so one cannot smuggle
    /// an extra `.` (which would let `"a.b"` masquerade as owning the
    /// sub-namespace `plugin.a.b.*` rather than the single segment
    /// `plugin.a.b`) or a `*` (which would let an id collide with a
    /// namespace-wildcard subscription like `plugin.a.*`).
    #[error(
        "plugin id {id:?} contains {ch:?} at byte {at}, which is not allowed — ids are \
         restricted to lowercase ascii letters, digits, '-' and '_' so an id can never \
         smuggle a namespace boundary ('.') or a wildcard ('*')"
    )]
    InvalidIdChar { id: String, ch: char, at: usize },
    /// The segment after `plugin.<id>.` must name something too.
    #[error("plugin event local name must not be empty")]
    EmptyLocalName,
    /// `*` is rejected in the local segment for the same reason as the id:
    /// a fact name is not a subscription pattern.
    #[error(
        "plugin event local name {local:?} contains '*' at byte {at}, which is not allowed — \
         a wildcard cannot appear in a concrete event name"
    )]
    WildcardInLocalName { local: String, at: usize },
}

fn is_valid_plugin_id_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'
}

/// Validate a plugin id on its own — the check [`plugin_event_name`] runs
/// before it will build a name from the id, and the check anything wanting
/// to confirm ownership without building a name (e.g. a lookup keyed by id)
/// should run first.
pub fn validate_plugin_id(id: &str) -> Result<(), PluginNamespaceError> {
    if id.is_empty() {
        return Err(PluginNamespaceError::EmptyId);
    }
    match id
        .char_indices()
        .find(|(_, c)| !is_valid_plugin_id_char(*c))
    {
        Some((at, ch)) => Err(PluginNamespaceError::InvalidIdChar {
            id: id.to_string(),
            ch,
            at,
        }),
        None => Ok(()),
    }
}

/// Build the one name a plugin owns for a fact of its own: `plugin.<id>.<local>`.
/// The sole constructor for a name in this namespace — go through it and the
/// two collisions this namespace exists to prevent (an id smuggling a `.` or
/// `*`, or an empty segment) cannot be represented in the string it returns.
pub fn plugin_event_name(plugin_id: &str, local: &str) -> Result<String, PluginNamespaceError> {
    validate_plugin_id(plugin_id)?;
    if local.is_empty() {
        return Err(PluginNamespaceError::EmptyLocalName);
    }
    if let Some(at) = local.find('*') {
        return Err(PluginNamespaceError::WildcardInLocalName {
            local: local.to_string(),
            at,
        });
    }
    Ok(format!("{PLUGIN_NAMESPACE_PREFIX}{plugin_id}.{local}"))
}

/// Whether `name` is owned by `plugin_id` — i.e. starts with exactly
/// `plugin.<plugin_id>.`. Rejects a name owned by a *different* plugin whose
/// id happens to share a prefix (`plugin_id` `"foo"` never matches
/// `"plugin.foobar.x"`): the boundary check requires the character right
/// after the id to be the separating dot, not just a shared prefix.
pub fn is_plugin_owned(name: &str, plugin_id: &str) -> bool {
    if validate_plugin_id(plugin_id).is_err() {
        return false;
    }
    name.strip_prefix(PLUGIN_NAMESPACE_PREFIX)
        .and_then(|rest| rest.strip_prefix(plugin_id))
        .is_some_and(|rest| rest.starts_with('.'))
}

/// The plugin id owning `name`, when `name` is a well-formed
/// `plugin.<id>.<local>` name. `None` for anything outside the namespace,
/// and also for a malformed member of it (`"plugin."`, `"plugin..x"`, an id
/// with an invalid character) — a fold reading the journal must not
/// misattribute a malformed name to a guessed id.
pub fn plugin_id_of(name: &str) -> Option<&str> {
    let rest = name.strip_prefix(PLUGIN_NAMESPACE_PREFIX)?;
    let (id, local) = rest.split_once('.')?;
    if local.is_empty() {
        return None;
    }
    validate_plugin_id(id).ok()?;
    Some(id)
}

#[cfg(test)]
mod plugin_namespace_tests {
    use super::*;

    #[test]
    fn plugin_names_never_collide_with_the_host_catalog() {
        for name in ALL {
            assert!(
                !name.starts_with(PLUGIN_NAMESPACE_PREFIX),
                "host-emitted name {name:?} must never start with {PLUGIN_NAMESPACE_PREFIX:?} — \
                 the namespace is reserved for plugins"
            );
        }
        for name in BLOCKING {
            assert!(!name.starts_with(PLUGIN_NAMESPACE_PREFIX));
        }
    }

    #[test]
    fn builds_and_recovers_a_well_formed_name() {
        let name = plugin_event_name("demo-reviewer_2", "finding.raised").unwrap();
        assert_eq!(name, "plugin.demo-reviewer_2.finding.raised");
        assert_eq!(plugin_id_of(&name), Some("demo-reviewer_2"));
        assert!(is_plugin_owned(&name, "demo-reviewer_2"));
    }

    #[test]
    fn rejects_a_dot_smuggled_into_the_id() {
        let err = plugin_event_name("a.b", "fact").unwrap_err();
        assert!(matches!(
            err,
            PluginNamespaceError::InvalidIdChar { ch: '.', .. }
        ));
    }

    #[test]
    fn rejects_a_wildcard_smuggled_into_the_id() {
        let err = plugin_event_name("a*", "fact").unwrap_err();
        assert!(matches!(
            err,
            PluginNamespaceError::InvalidIdChar { ch: '*', .. }
        ));
    }

    #[test]
    fn rejects_a_wildcard_in_the_local_segment() {
        let err = plugin_event_name("demo", "fact.*").unwrap_err();
        assert!(matches!(
            err,
            PluginNamespaceError::WildcardInLocalName { .. }
        ));
    }

    #[test]
    fn rejects_empty_id_and_empty_local() {
        assert_eq!(
            plugin_event_name("", "fact").unwrap_err(),
            PluginNamespaceError::EmptyId
        );
        assert_eq!(
            plugin_event_name("demo", "").unwrap_err(),
            PluginNamespaceError::EmptyLocalName
        );
    }

    /// A name outside a plugin's own namespace is rejected: neither
    /// ownership nor id recovery may be fooled by a shared string prefix
    /// that is not a shared namespace segment.
    #[test]
    fn a_name_outside_a_plugins_own_namespace_is_rejected() {
        let mine = plugin_event_name("foo", "fact").unwrap();
        assert!(is_plugin_owned(&mine, "foo"));
        assert!(!is_plugin_owned(&mine, "other"));
        // "foobar" is not the owner of a name in "foo"'s namespace, even
        // though the id string shares a prefix.
        assert!(!is_plugin_owned("plugin.foobar.fact", "foo"));
        assert_eq!(plugin_id_of("plugin.foobar.fact"), Some("foobar"));
        assert_eq!(plugin_id_of("not.a.plugin.name"), None);
        assert_eq!(plugin_id_of("plugin."), None);
        assert_eq!(plugin_id_of("plugin.."), None);
        assert_eq!(plugin_id_of("plugin.a.b"), Some("a"));
    }
}
