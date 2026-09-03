//! What the client does when a live server turns down a call for want of a
//! login.
//!
//! There are two ways in. The user takes back the grant mid-session. Or the
//! stored login runs out and the refresh comes back `invalid_grant`.
//!
//! Either way the wire error is [`McpError::Auth`]. The health module's
//! `is_connection_death` does not match it, so it lands on the `Protocol`
//! arm next to a `-32602`. That arm passes the error up and leaves health
//! alone. So the deck kept the server at [`crate::client::HealthState::Live`],
//! the state that means a call sent now would come back, while every call
//! failed. Each try also asked a third-party token endpoint again, with no
//! cache and no wait. A 401 at connect time has [`crate::suppress`] for
//! that. The one mid-session had nothing.
//!
//! The transport stays up. The link works; only the token is stale. A fresh
//! dial would carry the same dead token and cost a handshake to learn it.

use crate::error::McpError;

use super::health::Connection;

/// Pass a protocol error up, but count it against health first when it is a
/// login refusal.
///
/// Every other `Protocol` error still arms nothing. The server answered, and
/// a turned-down request says nothing about the link. A `-32602` for a typo
/// in an argument would take the fixed retry down with it.
pub(super) fn note_protocol_error(conn: &mut Connection, err: McpError) -> McpError {
    if err.is_auth_error() {
        conn.note_auth_failure(&err);
    }
    err
}

/// What to answer a request with while the hold-off window from an earlier
/// login failure still stands. `None` means a request may go out.
///
/// It takes the server by name, so it stays a plain function of its inputs.
/// A call that comes back clears the window. So does a fresh handshake. Both
/// are signed-in round trips, so either one proves the token works again.
pub(super) fn hold_off(server: &str, conn: &Connection) -> Option<McpError> {
    let left = conn.auth_blocked_for()?;
    let because = conn
        .health
        .last_error
        .as_deref()
        .map(|err| format!(" ({err})"))
        .unwrap_or_default();
    Some(McpError::Auth(format!(
        "server `{server}` refused the last call for want of authorization and is not being \
         asked again for {}s{because} — authorize with `stella mcp login {server}` (or `o` on \
         it in the deck's MCP tab)",
        left.as_secs(),
    )))
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::super::{HealthState, McpClient};
    use crate::error::McpError;
    use crate::transport::testkit::ScriptedTransport;

    /// The taken-back grant, end to end. The fix has two halves, so this
    /// checks both. Health must stop saying `Live`. And the next call must
    /// not reach the transport at all, which only the request log can show.
    #[tokio::test]
    async fn a_revoked_grant_marks_the_server_auth_required_and_holds_off_the_next_call() {
        let transport = ScriptedTransport::new();
        transport.push_err(
            "tools/call",
            McpError::Auth("token refresh failed: invalid_grant".into()),
        );
        let sent = transport.requests_handle();
        let client = McpClient::new("srv", Box::new(transport));

        let first = client.call_tool("t", Value::Null).await.unwrap_err();
        assert!(first.is_auth_error(), "{first:?}");

        let health = client.health().await;
        assert_eq!(health.state, HealthState::AuthRequired);
        assert_eq!(health.call_failures, 1);
        assert!(
            health.last_error.unwrap().contains("invalid_grant"),
            "the refusal itself is what the deck shows"
        );
        // No redial is pending. The transport was never torn down.
        assert!(health.retry_in.is_none());

        let second = client.call_tool("t", Value::Null).await.unwrap_err();
        assert!(second.is_auth_error(), "{second:?}");
        assert!(
            second.to_string().contains("stella mcp login srv"),
            "the refusal names the remedy: {second}"
        );
        assert_eq!(
            sent.lock().unwrap().len(),
            1,
            "a held-off call must not reach the server, or the token \
             endpoint gets one POST per try"
        );
    }

    /// A read-only method goes through the same door, so it holds off too.
    #[tokio::test]
    async fn a_resource_read_is_held_off_by_the_same_window() {
        let transport = ScriptedTransport::new();
        transport.push_err("resources/list", McpError::Auth("invalid_grant".into()));
        let sent = transport.requests_handle();
        let client = McpClient::new("srv", Box::new(transport));

        client.list_resources(None).await.unwrap_err();
        assert_eq!(client.health().await.state, HealthState::AuthRequired);

        client.list_resources(None).await.unwrap_err();
        assert_eq!(sent.lock().unwrap().len(), 1);
    }

    /// The other side of the arm. A plain turned-down request says nothing
    /// about the link, so it must arm nothing.
    #[tokio::test]
    async fn a_rejected_request_still_leaves_the_server_live() {
        let transport = ScriptedTransport::new();
        for _ in 0..2 {
            transport.push_err(
                "tools/call",
                McpError::JsonRpc {
                    code: -32602,
                    message: "unknown argument `pth`".into(),
                    data: None,
                },
            );
        }
        let sent = transport.requests_handle();
        let client = McpClient::new("srv", Box::new(transport));

        client.call_tool("t", Value::Null).await.unwrap_err();
        assert_eq!(client.health().await.state, HealthState::Live);
        client.call_tool("t", Value::Null).await.unwrap_err();
        assert_eq!(sent.lock().unwrap().len(), 2, "both attempts were sent");
    }
}
