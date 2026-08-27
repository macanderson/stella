//! Tests for the two session gates over a connected server's tools — the
//! operator's enable/disable toggle and the first-enable capability grant
//! (SPEC §9.3) — plus the connect-time latency the MCP tab renders beside them.
//!
//! Its own file because `tests.rs` is at the 1500-line ceiling, and because the
//! witness below needs something the rest of that file does not: a handle on
//! the transport's request log. Every other assertion there is about what came
//! back; this one is about what never went out.

use super::super::*;
use super::connected_client;
use crate::protocol::PREFERRED_PROTOCOL_VERSION;
use crate::transport::testkit::{Log, ScriptedTransport};

/// A connected client whose transport log stays readable, so a test can ask
/// what actually went over the wire — not merely what came back.
///
/// [`connected_client`] drops its `ScriptedTransport` into the client, which
/// makes "the call succeeded" observable and "no call was made" unobservable.
/// The capability gate needs the second one.
async fn client_with_wire_log(name: &str, tool: &str) -> (McpClient, Log) {
    let transport = ScriptedTransport::new();
    transport.push_ok(
        "initialize",
        serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
    );
    transport.push_ok(
        "tools/list",
        serde_json::json!({ "tools": [{ "name": tool, "inputSchema": { "type": "object" } }] }),
    );
    // Queued, and never consumed while the server is ungranted — which is
    // what the assertions below are for.
    transport.push_ok(
        "tools/call",
        serde_json::json!({ "content": [{ "type": "text", "text": "mcp ran" }] }),
    );
    let log = transport.requests_handle();
    let mut client = McpClient::new(name, Box::new(transport));
    client.initialize().await.unwrap();
    (client, log)
}

/// How many `tools/call` requests have reached the server so far.
fn wire_calls(log: &Log) -> usize {
    log.lock()
        .unwrap()
        .iter()
        .filter(|(method, _)| method == "tools/call")
        .count()
}

/// **The witness (#5047).** SPEC §9.3: a server's declared capabilities are
/// shown before its first enable, and **no tool call happens before the
/// grant**.
///
/// The assertion that matters is the absence, not the refusal: a gate that
/// refuses the caller *after* the request has gone down the pipe has already
/// let the third-party server act, and every `ToolOutput::Error` assertion in
/// this crate would pass over that bug. So the transport's own request log is
/// the evidence — zero `tools/call` on the wire while ungranted, exactly one
/// after the grant.
///
/// The handshake itself is expected on the wire before the grant: `initialize`
/// and `tools/list` are how the declared capabilities the operator reviews
/// become knowable at all.
#[tokio::test]
async fn no_tool_call_reaches_an_ungranted_server_before_the_grant() {
    let (client, wire) = client_with_wire_log("files", "read").await;
    let grants: CapabilityGrants = Arc::new(Mutex::new(HashSet::new()));
    let set = McpToolSet::from_clients(vec![client]).with_capability_grants(grants.clone());

    // The handshake ran — that is what the operator is about to review — and
    // nothing has been called.
    assert!(
        !set.advertised_tools("files").is_empty(),
        "the declared capabilities must be knowable before the grant, or there is \
         nothing to review"
    );
    assert_eq!(wire_calls(&wire), 0);

    // Ungranted: the model is never told the tool exists...
    assert!(
        set.schemas().iter().all(|s| s.name != "mcp__files__read"),
        "an ungranted server must not be advertised: {:?}",
        set.schemas()
    );
    // ...and a call issued anyway is stopped.
    let refused = set.execute("mcp__files__read", &Value::Null).await;

    // THE security property, asserted before anything about the answer: the
    // stop happened on this side of the pipe, so the server was never asked
    // to do anything. A gate that refuses its caller only after the request
    // has gone out has already let a third party act, and every assertion
    // about the returned `ToolOutput` would pass over it.
    assert_eq!(
        wire_calls(&wire),
        0,
        "a tool call reached the server before the operator granted it: {:?}",
        wire.lock().unwrap()
    );

    // And the refusal names its remedy, since a tool error is often the only
    // place the user meets this state.
    match refused {
        ToolOutput::Error { message, class } => {
            assert_eq!(class, Some(stella_protocol::ErrorClass::RefusedByPolicy));
            assert!(message.contains("has not been granted"), "{message}");
            assert!(message.contains("stella mcp grant files"), "{message}");
        }
        other => panic!("expected the ungranted refusal, got {other:?}"),
    }

    // The grant lands live — no reconnect, no re-advertise round trip.
    grants.lock().unwrap().insert("files".to_string());
    assert!(
        set.schemas().iter().any(|s| s.name == "mcp__files__read"),
        "a granted server's tools must be advertised"
    );
    assert!(
        !set.execute("mcp__files__read", &Value::Null)
            .await
            .is_error(),
        "a granted server's tools must be callable"
    );
    assert_eq!(wire_calls(&wire), 1, "and exactly one call went out");
}

/// The gate is default-deny only where a host installs it. A set with no
/// grants configured is ungated — the shape every embedder and one-shot
/// caller with no human to ask gets, and the reason the field is an
/// `Option`, not a bare `HashSet`.
#[tokio::test]
async fn a_set_with_no_grants_configured_is_not_gated() {
    let client = connected_client("files", "read").await;
    let set = McpToolSet::from_clients(vec![client]);
    assert!(set.schemas().iter().any(|s| s.name == "mcp__files__read"));
}

/// A server that is both disabled and ungranted says it is disabled: the
/// toggle is the operator's most recent instruction, and sending them to
/// review a handshake for a server they just switched off is the wrong
/// remedy.
#[tokio::test]
async fn the_disable_outranks_the_grant_in_the_reason_given() {
    let client = connected_client("files", "read").await;
    let disabled: DisabledServers = Arc::new(Mutex::new(HashSet::new()));
    disabled.lock().unwrap().insert("files".to_string());
    let set = McpToolSet::from_clients(vec![client])
        .with_disabled_servers(disabled)
        .with_capability_grants(Arc::new(Mutex::new(HashSet::new())));

    match set.execute("mcp__files__read", &Value::Null).await {
        ToolOutput::Error { message, .. } => {
            assert!(message.contains("disabled"), "{message}");
            assert!(!message.contains("granted"), "{message}");
        }
        other => panic!("expected the disabled refusal, got {other:?}"),
    }
}

/// The connect-time latency measurement reaches the health snapshot the tab
/// renders, and a client that never handshook reports nothing rather than a
/// zero that would render as the nearest server on the list.
#[tokio::test]
async fn health_carries_the_handshake_latency_and_never_invents_one() {
    let client = connected_client("files", "read").await;
    let set = McpToolSet::from_clients(vec![client]);
    let health = set.health().await;
    assert!(
        health[0].latency.is_some(),
        "a connected server's `initialize` round trip is measured"
    );

    let never_dialed = McpClient::new("files", Box::new(ScriptedTransport::new()));
    assert!(
        never_dialed.health().await.latency.is_none(),
        "no handshake, no measurement — never a synthesized zero"
    );
}
