//! Unit tests for [`super::McpToolSet`] — namespacing, routing, native
//! fall-through, disabled servers, reconnect resilience, the sorted and
//! budgeted schema segment, and the Best-of-N `CandidateMcpView`. Split out
//! of `toolset.rs` as a sibling submodule (the `driver/settlement.rs`
//! pattern) to keep the parent under the file-size gate; `use super::*`
//! keeps every private item in scope.

use super::*;
use crate::client::MAX_TOOLS_PER_SERVER;
use crate::client::ingest::{
    MAX_TOOL_DESCRIPTION_CHARS, MAX_TOOL_RESULT_BYTES, MAX_TOOL_SCHEMA_BYTES,
};
use crate::error::McpError;
use crate::protocol::PREFERRED_PROTOCOL_VERSION;
use crate::transport::Transport;
use crate::transport::testkit::ScriptedTransport;

/// A fake native executor advertising one tool, `bash`.
struct FakeNative;
#[async_trait]
impl ToolExecutor for FakeNative {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "bash".into(),
            description: "run a command".into(),
            input_schema: serde_json::json!({ "type": "object" }),
            read_only: false,
            speculation_safe: false,
        }]
    }
    async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: format!("native ran {name}"),
        }
    }
    fn parallel_safe_names(&self) -> HashSet<String> {
        HashSet::from(["task".to_string()])
    }
}

/// Both wrappers forward the native layer's concurrency claim — the empty
/// default would silently serialize sibling spawns in any MCP-connected
/// session (and in every Best-of-N candidate).
#[tokio::test]
async fn parallel_safe_names_forward_from_the_native_layer() {
    let client = connected_client("files", "read").await;
    let set = Arc::new(McpToolSet::from_clients(vec![client]).wrapping(Arc::new(FakeNative)));
    assert!(
        set.parallel_safe_names().contains("task"),
        "the native layer's claim must survive the MCP set"
    );

    let view = set.for_candidates(Arc::new(FakeNative));
    assert!(
        view.parallel_safe_names().contains("task"),
        "and the candidate view forwards its own native layer's claim"
    );

    let bare = McpToolSet::from_clients(Vec::new());
    assert!(
        bare.parallel_safe_names().is_empty(),
        "no native layer, no claims"
    );
}

/// A transport that never answers `tools/call` — used to prove the
/// per-call timeout fires and names the server.
struct HangingTransport;
#[async_trait]
impl Transport for HangingTransport {
    async fn request(&self, method: &str, _params: Value) -> Result<Value, McpError> {
        if method == "tools/call" {
            // Never resolves within the test's short call timeout.
            std::future::pending::<()>().await;
            unreachable!()
        }
        // Handshake methods answer instantly.
        match method {
            "initialize" => {
                Ok(serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }))
            }
            "tools/list" => Ok(serde_json::json!({
                "tools": [{ "name": "slow", "inputSchema": { "type": "object" } }]
            })),
            _ => Ok(Value::Null),
        }
    }
    async fn notify(&self, _method: &str, _params: Value) -> Result<(), McpError> {
        Ok(())
    }
    async fn close(&self) -> Result<(), McpError> {
        Ok(())
    }
}

async fn connected_client(name: &str, tool: &str) -> McpClient {
    connected_client_with_schema(name, tool, serde_json::json!({ "type": "object" })).await
}

/// Like [`connected_client`], with a caller-chosen `inputSchema` — for
/// proving [`McpToolSet::prefetch_candidate_context`]'s zero-required-input
/// filter against a tool that DOES require one.
async fn connected_client_with_schema(
    name: &str,
    tool: &str,
    input_schema: Value,
) -> McpClient {
    let transport = ScriptedTransport::new();
    transport.push_ok(
        "initialize",
        serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
    );
    transport.push_ok(
        "tools/list",
        serde_json::json!({ "tools": [{ "name": tool, "inputSchema": input_schema }] }),
    );
    // Pre-queue a successful call for the routing test.
    transport.push_ok(
        "tools/call",
        serde_json::json!({ "content": [{ "type": "text", "text": "mcp ran" }] }),
    );
    let mut client = McpClient::new(name, Box::new(transport));
    client.initialize().await.unwrap();
    client
}

#[tokio::test]
async fn connected_names_lists_live_servers_in_order() {
    let a = connected_client("files", "read").await;
    let b = connected_client("search", "grep").await;
    let set = McpToolSet::from_clients(vec![a, b]);
    assert_eq!(set.connected_names(), vec!["files", "search"]);
    assert!(set.failed_servers().is_empty());
}

/// The advertised toolset must be byte-identical however the servers
/// happened to be ordered (#1848).
///
/// This list is serialized verbatim at position 0 of the prompt prefix and
/// prompt caching is a byte-level prefix match, so the question is not
/// "does it look the same" but "do two processes emit the same bytes".
/// `ToolRegistry::schemas` sorts its own tools for that reason; this
/// decorator appended after it without re-sorting, so the guarantee held
/// only as long as `self.clients` order and connection-completion order
/// happened to match between processes — which across a restart, or two
/// stella processes in one workspace inside the cache TTL, they need not.
///
/// Serialized rather than compared field by field: bytes are what the
/// cache matches on, so bytes are what the assertion should be about.
#[tokio::test]
async fn schemas_are_byte_identical_whatever_order_the_servers_connected_in() {
    let forward = McpToolSet::from_clients(vec![
        connected_client("alpha", "one").await,
        connected_client("zeta", "two").await,
    ])
    .wrapping(Arc::new(FakeNative));
    let reverse = McpToolSet::from_clients(vec![
        connected_client("zeta", "two").await,
        connected_client("alpha", "one").await,
    ])
    .wrapping(Arc::new(FakeNative));

    let bytes = |set: &McpToolSet| serde_json::to_string(&set.schemas()).expect("serialize");
    assert_eq!(
        bytes(&forward),
        bytes(&reverse),
        "two processes that connected the same servers in different orders must \
         advertise the same bytes, or they cannot share a prompt-cache entry"
    );

    // And the segment contract the doc comment states: native first, then
    // the MCP tools. A sort applied to the WHOLE list would also make the
    // assertion above pass while silently moving the native tools.
    let names: Vec<String> = forward.schemas().into_iter().map(|s| s.name).collect();
    let first_mcp = names
        .iter()
        .position(|n| n.starts_with(NS_PREFIX))
        .expect("the mcp segment exists");
    assert!(
        names[..first_mcp].iter().all(|n| !n.starts_with(NS_PREFIX)),
        "native tools must all precede the mcp segment: {names:?}"
    );
    assert_eq!(
        names[first_mcp..],
        ["mcp__alpha__one".to_string(), "mcp__zeta__two".to_string()],
        "the mcp segment is sorted by namespaced name"
    );
}

/// One chatty server cannot spend the whole prefix (#1856).
///
/// Ingest already caps each tool — 256 per server, 2,000 description
/// characters, a per-tool schema budget. None of them bounds the SUM, and
/// the sum is what lands at position 0 of every request: 256 × 2,000 is
/// ~512 KB of third-party prose from one server, which can exceed the
/// whole recall+memory+rules budget combined while every per-item cap
/// holds.
///
/// Driven through the real `tools/list` path rather than by constructing
/// schemas directly, so it exercises what a server actually sends.
#[tokio::test]
async fn one_chatty_server_cannot_spend_the_whole_prefix() {
    // 40 tools × ~2 KB of description — every per-tool cap satisfied.
    let big = "d".repeat(2_000);
    let tools: Vec<Value> = (0..40)
        .map(|i| {
            serde_json::json!({
                "name": format!("tool{i:02}"),
                "description": big,
                "inputSchema": { "type": "object" },
            })
        })
        .collect();
    let transport = ScriptedTransport::new();
    transport.push_ok(
        "initialize",
        serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
    );
    transport.push_ok("tools/list", serde_json::json!({ "tools": tools }));
    let mut client = McpClient::new("chatty", Box::new(transport));
    client.initialize().await.unwrap();
    let set = McpToolSet::from_clients(vec![client]);

    let advertised = ToolExecutor::schemas(&set);
    let bytes: usize = advertised.iter().map(schema_cost).sum();
    assert!(
        bytes <= MAX_SERVER_SCHEMA_BYTES + 2_100,
        "one server advertised {bytes} bytes, over the \
         {MAX_SERVER_SCHEMA_BYTES}-byte budget (the slack is one \
         over-budget final tool, which is admitted by design)"
    );
    assert!(
        !advertised.is_empty(),
        "the budget must trim a chatty server, not silence it"
    );

    // The cut is reported, not silent: an operator who wonders why a tool
    // is missing needs to be told, and the count is the whole diagnostic.
    let over = set.over_budget_servers();
    assert_eq!(over.len(), 1, "one server was trimmed: {over:?}");
    assert_eq!(over[0].0, "chatty");
    assert_eq!(
        over[0].1 + advertised.len(),
        40,
        "every tool is either advertised or counted as dropped: {over:?}"
    );

    // Deterministic, because the segment is sorted before it is budgeted —
    // truncating an unsorted list would make WHICH tools survive depend on
    // client order, losing the byte-stability the sort exists to buy.
    let names: Vec<&str> = advertised.iter().map(|s| s.name.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(
        names, sorted,
        "the surviving tools are the lexicographic prefix"
    );
}

/// A well-behaved server is untouched: the budget must be invisible for
/// the servers people actually run.
#[tokio::test]
async fn an_ordinary_server_is_not_trimmed() {
    let set = McpToolSet::from_clients(vec![connected_client("files", "read").await]);
    assert_eq!(ToolExecutor::schemas(&set).len(), 1);
    assert!(
        set.over_budget_servers().is_empty(),
        "nothing to report for a server that fits"
    );
}

#[tokio::test]
async fn schemas_namespace_mcp_tools_and_include_native() {
    let client = connected_client("files", "read").await;
    let set = McpToolSet::from_clients(vec![client]).wrapping(Arc::new(FakeNative));

    let names: HashSet<String> = set.schemas().into_iter().map(|s| s.name).collect();
    assert!(names.contains("mcp__files__read"), "namespaced MCP tool");
    assert!(names.contains("bash"), "native tool passes through");
    assert_eq!(names.len(), 2);
}

#[tokio::test]
async fn execute_routes_by_prefix_and_falls_through_to_native() {
    let client = connected_client("files", "read").await;
    let set = McpToolSet::from_clients(vec![client]).wrapping(Arc::new(FakeNative));

    // Routes to the MCP server.
    let mcp = set
        .execute("mcp__files__read", &serde_json::json!({ "path": "x" }))
        .await;
    assert_eq!(
        mcp,
        ToolOutput::Ok {
            content: "mcp ran".into()
        }
    );

    // Falls through to native.
    let native = set.execute("bash", &serde_json::json!({})).await;
    assert_eq!(
        native,
        ToolOutput::Ok {
            content: "native ran bash".into()
        }
    );
}

#[tokio::test]
async fn unknown_mcp_tool_is_an_error_not_a_fallthrough() {
    let client = connected_client("files", "read").await;
    let set = McpToolSet::from_clients(vec![client]).wrapping(Arc::new(FakeNative));
    let out = set.execute("mcp__files__missing", &Value::Null).await;
    match out {
        ToolOutput::Error { message } => assert!(message.contains("unknown MCP tool")),
        other => panic!("expected an error, got {other:?}"),
    }
}

#[tokio::test]
async fn unknown_native_tool_errors_when_no_native_configured() {
    let client = connected_client("files", "read").await;
    let set = McpToolSet::from_clients(vec![client]);
    let out = set.execute("bash", &Value::Null).await;
    assert!(out.is_error());
}

#[tokio::test]
async fn a_hung_server_times_out_naming_the_server_without_poisoning_native() {
    let mut client = McpClient::new("slowsrv", Box::new(HangingTransport));
    client.initialize().await.unwrap();
    let set = McpToolSet::from_clients(vec![client])
        .wrapping(Arc::new(FakeNative))
        .with_call_timeout(Duration::from_millis(50));

    // The hung MCP call times out with a server-named error…
    let hung = set.execute("mcp__slowsrv__slow", &Value::Null).await;
    match hung {
        ToolOutput::Error { message } => {
            assert!(message.contains("slowsrv"), "names the server: {message}");
            assert!(message.contains("timed out"));
        }
        other => panic!("expected a timeout error, got {other:?}"),
    }

    // …and the native tool still works — the dead server didn't poison it.
    let native = set.execute("bash", &Value::Null).await;
    assert_eq!(
        native,
        ToolOutput::Ok {
            content: "native ran bash".into()
        }
    );
}

#[tokio::test]
async fn duplicate_tool_names_from_one_server_are_advertised_once_first_wins() {
    // One server advertising the same name twice. Both occurrences map to
    // the single `mcp__files__read` route, so without ingest dedupe the
    // second entry's only effect was a duplicate `ToolSchema` in every
    // model request.
    let transport = ScriptedTransport::new();
    transport.push_ok(
        "initialize",
        serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
    );
    transport.push_ok(
        "tools/list",
        serde_json::json!({ "tools": [
            { "name": "read", "description": "the first read",
              "inputSchema": { "type": "object" } },
            { "name": "read", "description": "a re-advertised impostor",
              "inputSchema": { "type": "object" } },
            { "name": "write", "inputSchema": { "type": "object" } },
        ] }),
    );
    transport.push_ok(
        "tools/call",
        serde_json::json!({ "content": [{ "type": "text", "text": "routed" }] }),
    );
    let mut client = McpClient::new("files", Box::new(transport));
    client.initialize().await.unwrap();
    // Deduped at ingest — the client never holds the second entry, so
    // routing, schemas, and retry-safety lookups all agree on one tool…
    assert_eq!(client.tools().len(), 2);

    let set = McpToolSet::from_clients(vec![client]);
    let schemas = set.schemas();
    assert_eq!(schemas.len(), 2);
    let read: Vec<_> = schemas
        .iter()
        .filter(|s| s.name == "mcp__files__read")
        .collect();
    assert_eq!(read.len(), 1, "one schema per advertised name");
    assert_eq!(
        read[0].description, "the first read",
        "the FIRST occurrence wins — same policy as duplicate server names"
    );

    // …and the kept entry still routes normally.
    assert_eq!(
        set.execute("mcp__files__read", &Value::Null).await,
        ToolOutput::Ok {
            content: "routed".into()
        }
    );
}

#[tokio::test]
async fn duplicate_and_invalid_server_names_are_recorded_not_advertised() {
    let a = connected_client("dup", "x").await;
    let b = connected_client("dup", "y").await;
    let bad = connected_client("has__sep", "z").await;
    let set = McpToolSet::from_clients(vec![a, b, bad]);

    assert_eq!(set.connected_count(), 1, "only the first `dup` is kept");
    let reasons: Vec<&str> = set
        .failed_servers()
        .iter()
        .map(|(_, r)| r.as_str())
        .collect();
    assert!(reasons.iter().any(|r| r.contains("duplicate")));
    assert!(reasons.iter().any(|r| r.contains("reserved")));
}

#[tokio::test]
async fn usage_ledger_records_a_successful_call_with_server_tool_and_reason() {
    let client = connected_client("files", "read").await;
    let ledger: McpUsageLedger = Arc::default();
    let set = McpToolSet::from_clients(vec![client]).with_usage_ledger(ledger.clone());

    let out = set
        .execute(
            "mcp__files__read",
            &serde_json::json!({ "reason": "inspect config" }),
        )
        .await;
    assert!(!out.is_error());

    let drained = stella_core::mcp_usage::drain_usage(&ledger);
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].server, "files");
    assert_eq!(drained[0].tool, "read");
    assert_eq!(drained[0].reason, "inspect config");
    assert!(drained[0].called_at_ms > 0);
}

#[tokio::test]
async fn disabled_server_is_hidden_from_schemas_and_errors_on_execute() {
    let client = connected_client("files", "read").await;
    let disabled: DisabledServers = Arc::new(Mutex::new(HashSet::new()));
    disabled.lock().unwrap().insert("files".to_string());
    let set = McpToolSet::from_clients(vec![client]).with_disabled_servers(disabled.clone());

    // Hidden from the advertised schema while disabled.
    assert!(
        set.schemas().iter().all(|s| s.name != "mcp__files__read"),
        "disabled server's tool must not be advertised"
    );
    // And a direct call errors, naming the disabled server.
    match set.execute("mcp__files__read", &Value::Null).await {
        ToolOutput::Error { message } => assert!(message.contains("disabled")),
        other => panic!("expected a disabled error, got {other:?}"),
    }

    // Re-enabling (clearing the set) makes the tool visible again — live,
    // no reconnect.
    disabled.lock().unwrap().clear();
    assert!(
        set.schemas().iter().any(|s| s.name == "mcp__files__read"),
        "re-enabled server's tool must reappear"
    );
}

// for_candidates / CandidateMcpView (issue #248 Phase 1)

#[tokio::test]
async fn candidate_view_advertises_only_allowlisted_mcp_tools_plus_native() {
    let docs = connected_client("docs", "search").await;
    let fs = connected_client("fs", "write").await;
    let set = Arc::new(
        McpToolSet::from_clients(vec![docs, fs]).with_candidate_safe_servers(["docs"]),
    );
    let view = set.for_candidates(Arc::new(FakeNative));

    let names: HashSet<String> = view.schemas().into_iter().map(|s| s.name).collect();
    assert!(names.contains("mcp__docs__search"), "allowlisted server");
    assert!(names.contains("bash"), "candidate's own native tool");
    assert!(
        !names.contains("mcp__fs__write"),
        "non-candidate_safe server must not be advertised: {names:?}"
    );
    assert_eq!(names.len(), 2);
}

#[tokio::test]
async fn candidate_view_executes_an_allowlisted_tool_and_falls_through_to_native() {
    let docs = connected_client("docs", "search").await;
    let set =
        Arc::new(McpToolSet::from_clients(vec![docs]).with_candidate_safe_servers(["docs"]));
    let view = set.for_candidates(Arc::new(FakeNative));

    let mcp_out = view.execute("mcp__docs__search", &Value::Null).await;
    assert_eq!(
        mcp_out,
        ToolOutput::Ok {
            content: "mcp ran".into()
        }
    );
    let native_out = view.execute("bash", &Value::Null).await;
    assert_eq!(
        native_out,
        ToolOutput::Ok {
            content: "native ran bash".into()
        }
    );
}

#[tokio::test]
async fn candidate_view_denies_a_non_allowlisted_mcp_tool_with_a_named_error() {
    let fs = connected_client("fs", "write").await;
    // No `with_candidate_safe_servers` call — nothing is allowlisted.
    let set = Arc::new(McpToolSet::from_clients(vec![fs]));
    let view = set.for_candidates(Arc::new(FakeNative));

    match view.execute("mcp__fs__write", &Value::Null).await {
        ToolOutput::Error { message } => {
            assert!(message.contains("mcp__fs__write"), "{message}");
            assert!(message.contains("candidate_safe"), "{message}");
        }
        other => panic!("expected a withheld error, got {other:?}"),
    }
}

#[tokio::test]
async fn candidate_view_respects_the_session_disabled_set() {
    // A server can be candidate_safe AND toggled off mid-session — the
    // candidate view must honor the live disable, same as the main set.
    let docs = connected_client("docs", "search").await;
    let disabled: DisabledServers = Arc::new(Mutex::new(HashSet::new()));
    disabled.lock().unwrap().insert("docs".to_string());
    let set = Arc::new(
        McpToolSet::from_clients(vec![docs])
            .with_candidate_safe_servers(["docs"])
            .with_disabled_servers(disabled),
    );
    let view = set.for_candidates(Arc::new(FakeNative));

    assert!(
        view.schemas().iter().all(|s| s.name != "mcp__docs__search"),
        "disabled server must not be advertised even if candidate_safe"
    );
    assert!(
        view.execute("mcp__docs__search", &Value::Null)
            .await
            .is_error()
    );
}

// prefetch_candidate_context (issue #248 Phase 1)

#[tokio::test]
async fn prefetch_calls_only_candidate_safe_zero_arg_tools() {
    let docs = connected_client("docs", "search").await; // {} schema
    let ticket = connected_client_with_schema(
        "ticket",
        "get",
        serde_json::json!({ "type": "object", "required": ["id"] }),
    )
    .await;
    let fs = connected_client("fs", "write").await; // zero-arg, but NOT allowlisted
    let set = McpToolSet::from_clients(vec![docs, ticket, fs])
        .with_candidate_safe_servers(["docs", "ticket"]);

    let context = set
        .prefetch_candidate_context()
        .await
        .expect("docs has a zero-arg candidate_safe tool");
    assert!(context.contains("mcp__docs__search"), "{context}");
    assert!(
        !context.contains("mcp__ticket__get"),
        "a tool requiring input must never be called blind: {context}"
    );
    assert!(
        !context.contains("mcp__fs__write"),
        "a non-candidate_safe server must not be prefetched: {context}"
    );
}

#[tokio::test]
async fn prefetch_returns_none_when_nothing_is_safe_to_call() {
    let fs = connected_client("fs", "write").await;
    // No `with_candidate_safe_servers` call — nothing is allowlisted.
    let set = McpToolSet::from_clients(vec![fs]);
    assert!(set.prefetch_candidate_context().await.is_none());
}

// Context budgets (#551) — nothing an untrusted server pushes at the model
// may be unbounded. These drive the real `McpClient` state machine through
// the public `McpToolSet` surface, so they witness the cap *and* that the
// server keeps working with it applied.

/// A connected client whose single `tools/call` answers with `text`.
async fn client_answering(name: &str, tool: &str, text: &str) -> McpClient {
    let transport = ScriptedTransport::new();
    transport.push_ok(
        "initialize",
        serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
    );
    transport.push_ok(
        "tools/list",
        serde_json::json!({ "tools": [{ "name": tool, "inputSchema": { "type": "object" } }] }),
    );
    transport.push_ok(
        "tools/call",
        serde_json::json!({ "content": [{ "type": "text", "text": text }] }),
    );
    let mut client = McpClient::new(name, Box::new(transport));
    client.initialize().await.unwrap();
    client
}

#[tokio::test]
async fn an_oversized_tool_result_is_capped_middle_out_with_a_marker() {
    let flood = format!("HEAD{}TAIL", "x".repeat(MAX_TOOL_RESULT_BYTES * 3));
    let set = McpToolSet::from_clients(vec![client_answering("flood", "dump", &flood).await]);

    let out = set.execute("mcp__flood__dump", &Value::Null).await;
    let ToolOutput::Ok { content } = out else {
        panic!("expected a successful call, got {out:?}");
    };
    assert!(
        content.len() < MAX_TOOL_RESULT_BYTES + 128,
        "a {} byte result must not reach the model whole (got {})",
        flood.len(),
        content.len()
    );
    assert!(content.starts_with("HEAD"), "the head survives");
    assert!(content.ends_with("TAIL"), "the tail survives (L-S3)");
    assert!(
        content.contains("... [truncated ") && content.contains(" bytes] ..."),
        "the elision is explicit and counted"
    );
}

#[tokio::test]
async fn an_oversized_error_result_is_capped_too() {
    let transport = ScriptedTransport::new();
    transport.push_ok(
        "initialize",
        serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
    );
    transport.push_ok(
        "tools/list",
        serde_json::json!({ "tools": [{ "name": "boom", "inputSchema": { "type": "object" } }] }),
    );
    transport.push_ok(
        "tools/call",
        serde_json::json!({
            "content": [{ "type": "text", "text": "e".repeat(MAX_TOOL_RESULT_BYTES * 2) }],
            "isError": true,
        }),
    );
    let mut client = McpClient::new("flood", Box::new(transport));
    client.initialize().await.unwrap();
    let set = McpToolSet::from_clients(vec![client]);

    match set.execute("mcp__flood__boom", &Value::Null).await {
        ToolOutput::Error { message } => assert!(
            message.len() < MAX_TOOL_RESULT_BYTES + 128,
            "an error message is model context too: {} bytes",
            message.len()
        ),
        other => panic!("expected a tool error, got {other:?}"),
    }
}

/// Witness for the hole PR #586 left in #551's context bound. The result
/// cap lives in `decode_call_result`, which a JSON-RPC **error object**
/// never reaches: the request fails first, and `execute_mcp`'s `Err` arm
/// used to interpolate the server's `error.message` verbatim. That is the
/// cheapest possible hostile payload — no content array, no `isError`,
/// just a huge string in the error field — and it landed in model context
/// whole. Distinct from `an_oversized_error_result_is_capped_too` above,
/// which exercises the `isError: true` *content* route.
#[tokio::test]
async fn an_oversized_json_rpc_error_message_is_capped_before_the_model() {
    let transport = ScriptedTransport::new();
    transport.push_ok(
        "initialize",
        serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
    );
    transport.push_ok(
        "tools/list",
        serde_json::json!({ "tools": [{ "name": "boom", "inputSchema": { "type": "object" } }] }),
    );
    transport.push_err(
        "tools/call",
        McpError::JsonRpc {
            code: -32000,
            message: "Z".repeat(MAX_TOOL_RESULT_BYTES * 10),
            data: None,
        },
    );
    let mut client = McpClient::new("flood", Box::new(transport));
    client.initialize().await.unwrap();
    let set = McpToolSet::from_clients(vec![client]);

    match set.execute("mcp__flood__boom", &Value::Null).await {
        ToolOutput::Error { message } => {
            assert!(
                message.len() < MAX_TOOL_RESULT_BYTES + 256,
                "a {}-byte server-chosen error message must not reach the model whole \
                 (got {} bytes)",
                MAX_TOOL_RESULT_BYTES * 10,
                message.len()
            );
            assert!(
                message.contains("mcp server `flood` failed calling `boom`"),
                "the server is still named"
            );
            assert!(
                message.contains("... [truncated ") && message.contains(" bytes] ..."),
                "the elision is explicit and counted"
            );
        }
        other => panic!("expected a tool error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_under_budget_json_rpc_error_message_is_passed_through_verbatim() {
    let transport = ScriptedTransport::new();
    transport.push_ok(
        "initialize",
        serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
    );
    transport.push_ok(
        "tools/list",
        serde_json::json!({ "tools": [{ "name": "boom", "inputSchema": { "type": "object" } }] }),
    );
    transport.push_err(
        "tools/call",
        McpError::JsonRpc {
            code: -32602,
            message: "unknown argument `pth`".into(),
            data: None,
        },
    );
    let mut client = McpClient::new("files", Box::new(transport));
    client.initialize().await.unwrap();
    let set = McpToolSet::from_clients(vec![client]);

    assert_eq!(
        set.execute("mcp__files__boom", &Value::Null).await,
        ToolOutput::Error {
            message: "mcp server `files` failed calling `boom`: json-rpc error -32602: \
                      unknown argument `pth`"
                .into()
        }
    );
}

#[tokio::test]
async fn an_under_budget_result_is_byte_identical() {
    let set = McpToolSet::from_clients(vec![
        client_answering("files", "read", "small, exact, unmodified").await,
    ]);
    assert_eq!(
        set.execute("mcp__files__read", &Value::Null).await,
        ToolOutput::Ok {
            content: "small, exact, unmodified".into()
        }
    );
}

#[tokio::test]
async fn a_multibyte_result_is_capped_without_slicing_a_codepoint() {
    // Every candidate cut point falls mid-codepoint for a 3-byte char.
    let flood = "漢".repeat(MAX_TOOL_RESULT_BYTES);
    let set = McpToolSet::from_clients(vec![client_answering("cjk", "dump", &flood).await]);

    let ToolOutput::Ok { content } = set.execute("mcp__cjk__dump", &Value::Null).await else {
        panic!("expected a successful call");
    };
    assert!(content.len() < MAX_TOOL_RESULT_BYTES + 128);
    assert!(
        content.chars().all(|c| c == '漢' || c.is_ascii()),
        "no codepoint may be sliced apart"
    );
}

#[tokio::test]
async fn tools_past_the_per_server_cap_are_dropped_recorded_and_the_server_still_works() {
    let overflow = 7;
    let advertised: Vec<Value> = (0..MAX_TOOLS_PER_SERVER + overflow)
        .map(|i| serde_json::json!({ "name": format!("t{i}"), "inputSchema": { "type": "object" } }))
        .collect();
    let transport = ScriptedTransport::new();
    transport.push_ok(
        "initialize",
        serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
    );
    transport.push_ok("tools/list", serde_json::json!({ "tools": advertised }));
    transport.push_ok(
        "tools/call",
        serde_json::json!({ "content": [{ "type": "text", "text": "still routing" }] }),
    );
    let mut client = McpClient::new("greedy", Box::new(transport));
    client.initialize().await.unwrap();
    assert_eq!(client.tools().len(), MAX_TOOLS_PER_SERVER);
    assert_eq!(client.dropped_tool_count(), overflow);

    let set = McpToolSet::from_clients(vec![client]);
    // The overflow is a diagnostic of its own — NOT `failed_servers`,
    // which callers render as "server unavailable".
    assert!(set.failed_servers().is_empty());
    assert_eq!(set.over_advertising_servers(), vec![("greedy", overflow)]);

    // Routes and schemas agree with the truncated tool list…
    assert_eq!(
        set.schemas()
            .iter()
            .filter(|s| s.name.starts_with("mcp__greedy__"))
            .count(),
        MAX_TOOLS_PER_SERVER
    );
    // A tool the server DID advertise, past the cap, is neither advertised
    // nor callable.
    let dropped = format!("mcp__greedy__t{}", MAX_TOOLS_PER_SERVER + 1);
    assert!(
        set.execute(&dropped, &Value::Null).await.is_error(),
        "a dropped tool is not callable"
    );
    // …and a kept tool still calls through normally.
    assert_eq!(
        set.execute("mcp__greedy__t0", &Value::Null).await,
        ToolOutput::Ok {
            content: "still routing".into()
        }
    );
}

#[tokio::test]
async fn a_well_behaved_server_records_no_overflow() {
    let set = McpToolSet::from_clients(vec![connected_client("files", "read").await]);
    assert!(set.over_advertising_servers().is_empty());
    assert_eq!(set.dropped_tool_count(), 0);
}

/// A connected client that advertised `overflow` tools past the cap.
async fn over_advertising_client(name: &str, overflow: usize) -> McpClient {
    let advertised: Vec<Value> = (0..MAX_TOOLS_PER_SERVER + overflow)
        .map(|i| serde_json::json!({ "name": format!("t{i}"), "inputSchema": { "type": "object" } }))
        .collect();
    let transport = ScriptedTransport::new();
    transport.push_ok(
        "initialize",
        serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
    );
    transport.push_ok("tools/list", serde_json::json!({ "tools": advertised }));
    let mut client = McpClient::new(name, Box::new(transport));
    client.initialize().await.unwrap();
    client
}

/// #638: the truncation has to be legible to an operator, which means one
/// number for the session and one record per server — never folded into
/// `failed_servers`, which is rendered as "server unavailable" and would be
/// a lie about a server that is up and answering.
#[tokio::test]
async fn truncation_is_reported_per_server_and_as_a_session_total() {
    let set = McpToolSet::from_clients(vec![
        over_advertising_client("greedy", 7).await,
        connected_client("files", "read").await,
        over_advertising_client("greedier", 40).await,
    ]);

    assert_eq!(
        set.over_advertising_servers(),
        vec![("greedy", 7), ("greedier", 40)],
        "the bounded record names only the servers that truncated"
    );
    assert_eq!(
        set.dropped_tool_count(),
        47,
        "…and the total is the one number a status line can carry"
    );
    assert!(
        set.failed_servers().is_empty(),
        "a chatty server is NOT an unavailable one"
    );
    assert_eq!(
        set.connected_count(),
        3,
        "every server, truncated or not, is connected and routing"
    );
}

#[tokio::test]
async fn an_oversized_description_and_schema_are_bounded_at_ingest() {
    // One tool with a ~1 MB description, one with a ~1 MB schema.
    let fat_schema = serde_json::json!({
        "type": "object",
        "properties": { "blob": { "type": "string", "description": "y".repeat(MAX_TOOL_SCHEMA_BYTES * 64) } },
    });
    let transport = ScriptedTransport::new();
    transport.push_ok(
        "initialize",
        serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
    );
    transport.push_ok(
        "tools/list",
        serde_json::json!({ "tools": [
            { "name": "wordy", "description": "z".repeat(1_000_000),
              "inputSchema": { "type": "object" } },
            { "name": "fat", "description": "short", "inputSchema": fat_schema },
        ] }),
    );
    let mut client = McpClient::new("verbose", Box::new(transport));
    client.initialize().await.unwrap();
    let set = McpToolSet::from_clients(vec![client]);

    let schemas = set.schemas();
    let wordy = schemas
        .iter()
        .find(|s| s.name == "mcp__verbose__wordy")
        .expect("wordy is advertised");
    assert!(
        wordy.description.chars().count() <= MAX_TOOL_DESCRIPTION_CHARS + 1,
        "a megabyte description must not reach the model: {} chars",
        wordy.description.chars().count()
    );

    let fat = schemas
        .iter()
        .find(|s| s.name == "mcp__verbose__fat")
        .expect("fat is advertised");
    assert!(
        serde_json::to_vec(&fat.input_schema).unwrap().len() <= MAX_TOOL_SCHEMA_BYTES,
        "an over-budget schema must be bounded"
    );
    // Bounded *and still valid JSON Schema* — a chopped string would not be.
    assert_eq!(fat.input_schema, serde_json::json!({ "type": "object" }));
    assert!(
        fat.description.contains("permissive"),
        "the model is told its schema was replaced: {}",
        fat.description
    );
}
