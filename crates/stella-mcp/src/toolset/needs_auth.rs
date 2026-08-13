// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The synthetic `login_required` tool an auth-suppressed server advertises
//! in place of its real ones (#2687).
//!
//! When [`super::McpToolSet::connect_with_auth`] skips (or fails) a server
//! because it requires authentication the user has not granted
//! ([`crate::suppress`]), the model would otherwise see *nothing* — the
//! server's tools silently absent, with no way to learn why or to tell the
//! user how to fix it. Instead the set advertises exactly one placeholder
//! per such server, named with the same `mcp__<server>__<tool>` convention
//! as every real MCP tool, whose description and invocation both carry the
//! `stella mcp login <server>` instruction. Invoking it touches nothing —
//! it returns the instruction as a model-visible `ToolOutput::Ok` — which is
//! why its schema is `read_only: true` (advertisable on read-only surfaces),
//! the one deliberate exception to "every MCP tool is treated as mutating":
//! this tool is authored *here*, not by the untrusted server.
//!
//! It is deliberately invisible to Best-of-N candidates: it never enters the
//! routing map, so [`super::McpToolSet::is_candidate_safe_tool`] answers
//! `false` and [`super::CandidateMcpView`] filters it out — a candidate
//! cannot act on a login instruction, only the user can.
//!
//! Everything here is deterministic prose over the server name, so the
//! advertised schema is byte-stable across sessions (invariant #7's
//! discipline applied to the tool surface).

use stella_protocol::{ToolOutput, ToolSchema};

use super::wire_name;
use crate::client::{HealthState, ServerHealth};

/// The raw (un-namespaced) tool-name segment.
const LOGIN_REQUIRED_TOOL: &str = "login_required";

/// The namespaced synthetic tool name for `server`:
/// `mcp__<server>__login_required` — the same convention as every real MCP
/// tool, so per-tool policy and UI grouping treat it as the server's.
pub fn login_required_tool_name(server: &str) -> String {
    wire_name(server, LOGIN_REQUIRED_TOOL)
}

/// Append each auth-suppressed server's placeholder schema — honoring the
/// session's live disable, exactly like a real server's tools.
pub(super) fn extend_schemas(set: &super::McpToolSet, schemas: &mut Vec<ToolSchema>) {
    for (name, _) in &set.auth_required {
        if !set.is_disabled(name) {
            schemas.push(login_required_schema(name));
        }
    }
}

/// Route `name` if it is some auth-suppressed server's synthetic tool: the
/// login instruction — or the session-disabled error, matching how a real
/// server's tools answer while disabled. `None` for every other name.
pub(super) fn route(set: &super::McpToolSet, name: &str) -> Option<ToolOutput> {
    let (server, _) = set
        .auth_required
        .iter()
        .find(|(server, _)| name == login_required_tool_name(server))?;
    if set.is_disabled(server) {
        return Some(ToolOutput::error(format!(
            "mcp server `{server}` is disabled for this session — tool `{name}` unavailable"
        )));
    }
    Some(login_required_output(server))
}

/// The one schema an auth-suppressed `server` advertises.
pub(super) fn login_required_schema(server: &str) -> ToolSchema {
    ToolSchema {
        name: login_required_tool_name(server),
        description: format!(
            "MCP server `{server}` is configured but requires authentication, so its tools \
             are unavailable this session. Call this to get the instruction for granting \
             access (the user must run `stella mcp login {server}`)."
        ),
        input_schema: serde_json::json!({ "type": "object", "properties": {} }),
        // Read-only: invoking it touches nothing (the instruction below is
        // authored by this crate, not fetched from the server), so it stays
        // advertisable on read-only surfaces. Not speculation-safe only
        // because speculating a no-op buys nothing.
        read_only: true,
        speculation_safe: false,
    }
}

/// The model-visible result of invoking the synthetic tool.
pub(super) fn login_required_output(server: &str) -> ToolOutput {
    ToolOutput::Ok {
        content: format!(
            "MCP server `{server}` requires authentication before its tools can be used. \
             Ask the user to run `stella mcp login {server}` in a terminal (or press `o` \
             on the server in the deck's MCP tab). Once they have logged in, the server \
             connects and its real tools replace this one on the next session start."
        ),
    }
}

/// The synthesized health row for an auth-suppressed server, appended by
/// [`super::McpToolSet::health`]. Zero failure counters and no retry clock:
/// nothing was dialed and nothing will be until the user logs in or the
/// suppression TTL lapses ([`crate::suppress::AUTH_PROBE_TTL`]).
pub(super) fn auth_required_health(server: &str, reason: &str) -> ServerHealth {
    ServerHealth {
        name: server.to_string(),
        state: HealthState::AuthRequired,
        call_failures: 0,
        connect_failures: 0,
        last_error: Some(reason.to_string()),
        retry_in: None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{DisabledServers, McpToolSet};
    use super::*;
    use crate::client::ingest::MAX_TOOL_DESCRIPTION_CHARS;
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    /// An empty set with `server` marked auth-required — the shape
    /// `connect_with_auth` produces for a suppressed server, built directly
    /// so these tests need no socket.
    fn suppressed_set(server: &str) -> McpToolSet {
        let mut set = McpToolSet::from_clients(vec![]);
        set.auth_required.push((
            server.to_string(),
            format!("server `{server}` refused the last connect with HTTP 401"),
        ));
        set
    }

    #[test]
    fn the_synthetic_name_follows_the_mcp_namespace_convention() {
        assert_eq!(
            login_required_tool_name("github"),
            "mcp__github__login_required"
        );
    }

    #[test]
    fn the_schema_is_read_only_bounded_and_names_the_login_command() {
        let schema = login_required_schema("github");
        assert!(
            schema.read_only,
            "must be advertisable on read-only surfaces"
        );
        assert!(!schema.speculation_safe);
        assert!(
            schema.description.contains("stella mcp login github"),
            "{}",
            schema.description
        );
        assert!(
            schema.description.chars().count() <= MAX_TOOL_DESCRIPTION_CHARS,
            "the synthetic tool honors the same ingest budget as real ones"
        );
        // Callable with `{}` — it takes no input.
        assert!(super::super::accepts_empty_input(&schema.input_schema));
        // Deterministic: the advertised surface is byte-stable per server.
        assert_eq!(schema, login_required_schema("github"));
    }

    #[tokio::test]
    async fn a_suppressed_server_advertises_one_tool_and_answers_the_instruction() {
        use stella_core::ports::ToolExecutor as _;
        let set = suppressed_set("github");

        let schemas = set.schemas();
        assert_eq!(schemas.len(), 1, "exactly one placeholder, no real tools");
        assert_eq!(schemas[0].name, "mcp__github__login_required");

        match set
            .execute("mcp__github__login_required", &serde_json::Value::Null)
            .await
        {
            ToolOutput::Ok { content } => {
                assert!(content.contains("stella mcp login github"), "{content}");
            }
            other => panic!("the instruction is a result, not an error: {other:?}"),
        }

        // Any OTHER tool of the suppressed server is still an unknown-tool
        // error — suppression advertises the placeholder, nothing else.
        assert!(
            set.execute("mcp__github__create_issue", &serde_json::Value::Null)
                .await
                .is_error()
        );
    }

    #[tokio::test]
    async fn health_reports_auth_required_with_the_reason() {
        let set = suppressed_set("github");
        let health = set.health().await;
        assert_eq!(health.len(), 1);
        assert_eq!(health[0].name, "github");
        assert_eq!(health[0].state, HealthState::AuthRequired);
        assert_eq!(health[0].call_failures, 0);
        assert_eq!(health[0].connect_failures, 0);
        assert!(health[0].retry_in.is_none(), "nothing is being redialed");
        assert!(
            health[0]
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("401")),
            "the reason rides along: {:?}",
            health[0].last_error
        );
    }

    #[tokio::test]
    async fn a_disabled_suppressed_server_hides_its_placeholder_too() {
        use stella_core::ports::ToolExecutor as _;
        let disabled: DisabledServers = Arc::new(Mutex::new(HashSet::new()));
        disabled.lock().unwrap().insert("github".to_string());
        let set = suppressed_set("github").with_disabled_servers(disabled.clone());

        assert!(
            set.schemas().is_empty(),
            "a server the operator switched off advertises nothing, placeholder included"
        );
        match set
            .execute("mcp__github__login_required", &serde_json::Value::Null)
            .await
        {
            ToolOutput::Error { message, .. } => assert!(message.contains("disabled"), "{message}"),
            other => panic!("expected the disabled error, got {other:?}"),
        }

        // Re-enabling brings the placeholder back, live.
        disabled.lock().unwrap().clear();
        assert_eq!(set.schemas().len(), 1);
    }

    #[test]
    fn the_placeholder_is_withheld_from_best_of_n_candidates() {
        let set = Arc::new(suppressed_set("github").with_candidate_safe_servers(["github"]));
        assert!(
            !set.is_candidate_safe_tool("mcp__github__login_required"),
            "a candidate cannot act on a login instruction — only the user can"
        );
    }
}
