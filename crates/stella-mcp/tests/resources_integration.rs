//! End-to-end witnesses for #2678 (MCP resources) over the real
//! `mcp-fixture-server` binary: an embedded text resource in a `tools/call`
//! result renders its text instead of a placeholder, a resources-capable
//! server advertises `list_resources`/`read_resource` as read-only tools that
//! round-trip on the wire, and a server without the capability advertises
//! neither. Deliberately written against the *pre-change* public surface
//! (`McpToolSet` + `ToolExecutor` only, the [`auth_probe_suppression`]
//! pattern), so the whole file compiles on the base commit and fails there on
//! behavior, not on a missing symbol.
//!
//! [`auth_probe_suppression`]: ../tests/auth_probe_suppression.rs

use std::collections::BTreeMap;
use std::time::Duration;

use stella_core::ports::ToolExecutor;
use stella_mcp::{McpServerConfig, McpToolSet, McpTransport};
use stella_protocol::ToolOutput;

/// The fixture server built by this crate.
const FIXTURE: &str = env!("CARGO_BIN_EXE_mcp-fixture-server");

fn stdio_config(name: &str, args: &[&str]) -> McpServerConfig {
    McpServerConfig {
        name: name.to_string(),
        transport: McpTransport::Stdio {
            cmd: FIXTURE.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: BTreeMap::new(),
        },
        candidate_safe: false,
    }
}

async fn connect(args: &[&str]) -> McpToolSet {
    let cfg = stdio_config("fx", args);
    let set = McpToolSet::connect(std::slice::from_ref(&cfg), Duration::from_secs(5)).await;
    assert_eq!(set.connected_count(), 1, "{:?}", set.failed_servers());
    set
}

/// #2678 witness (a): the fixture's `make_resource` tool answers with an
/// embedded resource `{ uri: file:///r.txt, text: "hi" }`. The text the
/// server chose to return must reach the model — it used to be degraded to
/// the `[resource: file:///r.txt]` placeholder at the client boundary.
#[tokio::test]
async fn an_embedded_text_resource_renders_its_text_not_a_placeholder() {
    let set = connect(&[]).await;
    match set
        .execute("mcp__fx__make_resource", &serde_json::json!({}))
        .await
    {
        ToolOutput::Ok { content } => {
            assert!(
                content.contains("hi"),
                "the resource's text must render inline: {content}"
            );
            assert!(
                content.contains("file:///r.txt"),
                "and the uri still names where it came from: {content}"
            );
        }
        other => panic!("expected Ok, got {other:?}"),
    }
    set.close_all().await;
}

/// #2678 witnesses (b, half) and (d): a server that declared the `resources`
/// capability advertises the two synthetic tools, both `read_only: true` and
/// not `speculation_safe`.
#[tokio::test]
async fn a_resources_capable_server_advertises_two_read_only_tools() {
    let set = connect(&[]).await;
    let schemas = set.schemas();
    for name in ["mcp__fx__list_resources", "mcp__fx__read_resource"] {
        let schema = schemas
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("`{name}` must be advertised, got {schemas:?}"));
        assert!(schema.read_only, "`{name}` is the read surface");
        assert!(
            !schema.speculation_safe,
            "`{name}`: server request budgets are not ours to spend twice"
        );
    }
    set.close_all().await;
}

/// #2678 witness (b): `resources/list` and `resources/read` round-trip
/// against the fixture server — the listing names the fixture's resources,
/// reading the text one inlines its contents, reading the binary one
/// summarizes without inlining base64, and an unknown URI is a model-visible
/// server-named error.
#[tokio::test]
async fn resources_list_and_read_round_trip() {
    let set = connect(&[]).await;

    match set
        .execute("mcp__fx__list_resources", &serde_json::json!({}))
        .await
    {
        ToolOutput::Ok { content } => {
            assert!(content.contains("file:///r.txt"), "{content}");
            assert!(content.contains("file:///data.bin"), "{content}");
        }
        other => panic!("expected the listing, got {other:?}"),
    }

    match set
        .execute(
            "mcp__fx__read_resource",
            &serde_json::json!({ "uri": "file:///r.txt" }),
        )
        .await
    {
        ToolOutput::Ok { content } => {
            assert!(
                content.contains("hello from the fixture resource"),
                "text contents render inline: {content}"
            );
        }
        other => panic!("expected the text read, got {other:?}"),
    }

    match set
        .execute(
            "mcp__fx__read_resource",
            &serde_json::json!({ "uri": "file:///data.bin" }),
        )
        .await
    {
        ToolOutput::Ok { content } => {
            assert!(content.contains("file:///data.bin"), "{content}");
            assert!(
                !content.contains("QUFBQQ=="),
                "base64 must never inline: {content}"
            );
        }
        other => panic!("expected the blob summary, got {other:?}"),
    }

    match set
        .execute(
            "mcp__fx__read_resource",
            &serde_json::json!({ "uri": "file:///missing.txt" }),
        )
        .await
    {
        ToolOutput::Error { message, .. } => {
            assert!(message.contains("fx"), "names the server: {message}");
            assert!(message.contains("missing.txt"), "names the uri: {message}");
        }
        other => panic!("expected the not-found error, got {other:?}"),
    }

    set.close_all().await;
}

/// #2678 witness (c): a server WITHOUT the `resources` capability advertises
/// neither tool, and calling one anyway is a model-visible unknown-tool
/// error. (The negative control — this also passes on the base commit.)
#[tokio::test]
async fn a_server_without_the_capability_advertises_no_resource_tools() {
    let set = connect(&["--no-resources"]).await;
    let names: Vec<String> = set.schemas().into_iter().map(|s| s.name).collect();
    assert!(
        !names
            .iter()
            .any(|n| n == "mcp__fx__list_resources" || n == "mcp__fx__read_resource"),
        "a server that never declared the capability must not be probed for it: {names:?}"
    );

    match set
        .execute("mcp__fx__list_resources", &serde_json::json!({}))
        .await
    {
        ToolOutput::Error { message, .. } => {
            assert!(message.contains("not advertised"), "{message}");
        }
        other => panic!("expected the unknown-tool error, got {other:?}"),
    }
    set.close_all().await;
}
