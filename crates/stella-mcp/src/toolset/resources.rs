// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The synthetic `list_resources` / `read_resource` tools a resources-capable
//! server advertises (#2678).
//!
//! MCP servers expose more than tools: a server that declared the `resources`
//! capability in its `initialize` result offers addressable data (files,
//! documents, schemas) over `resources/list` / `resources/read`. This module
//! surfaces that plane to the model as two native-looking tools per such
//! server — named with the same `mcp__<server>__<tool>` convention as every
//! real MCP tool — so per-tool policy and UI grouping treat them as the
//! server's. Two tools, not one with a mode flag: `list` and `read` are
//! different verbs, and a parameter may scope an operation (a cursor, a URI),
//! never select one (invariant #9).
//!
//! Both schemas are `read_only: true`: the verbs are the protocol's read
//! surface and the schemas are authored *here*, not by the untrusted server —
//! the same reasoning as [`super::needs_auth`]'s placeholder, and unlike the
//! server-advertised tools, whose unknown behavior keeps them `read_only:
//! false`. Neither is `speculation_safe`: the server's request budget and
//! rate limit are not ours to spend twice (#923).
//!
//! The payloads are still untrusted input, so they leave through the same
//! bounded door as every `tools/call` result: text renders through
//! [`render_resource_contents`] (base64 blobs summarized, never inlined) and
//! the whole rendered answer is capped at [`MAX_TOOL_RESULT_BYTES`]
//! middle-out (#551).
//!
//! A server whose *own* tool list already claims one of these wire names
//! keeps it: the synthetic tool is withheld ([`shadowed_by_real_tool`])
//! rather than advertised as a duplicate name or silently swapped in over a
//! contested (#2675) one.
//!
//! Like the [`super::needs_auth`] placeholder, these tools never enter the
//! routing map, so [`super::McpToolSet::is_candidate_safe_tool`] answers
//! `false` and they stay withheld from Best-of-N candidates.
//!
//! Everything advertised is deterministic prose over the server name, so the
//! schema surface is byte-stable across sessions (invariant #7's discipline
//! applied to the tool surface).

use serde_json::Value;
use stella_core::mcp_usage::{McpUsageRecord, push_usage};
use stella_protocol::{ToolOutput, ToolSchema};

use super::wire_name;
use crate::client::McpClient;
use crate::client::ingest::{MAX_TOOL_RESULT_BYTES, render_resource_contents};
use crate::http::{truncate, truncate_middle_out};
use crate::protocol::{ListResourcesResult, ReadResourceResult, ResourceInfo};

/// The raw (un-namespaced) tool-name segments.
const LIST_RESOURCES_TOOL: &str = "list_resources";
const READ_RESOURCE_TOOL: &str = "read_resource";

/// Character budget for one resource's description in a listing line. Far
/// tighter than ingest's 2 000-char tool-description budget on purpose: a
/// tool description is the model's whole contract for calling it, while a
/// listing line is an index entry — `read_resource` fetches the real thing.
const MAX_LIST_DESCRIPTION_CHARS: usize = 200;

/// The namespaced `mcp__<server>__list_resources` name.
pub fn list_resources_tool_name(server: &str) -> String {
    wire_name(server, LIST_RESOURCES_TOOL)
}

/// The namespaced `mcp__<server>__read_resource` name.
pub fn read_resource_tool_name(server: &str) -> String {
    wire_name(server, READ_RESOURCE_TOOL)
}

/// Append the two resource tools for every connected server that declared the
/// `resources` capability — honoring the session's live disable and standing
/// aside for a real tool that claims the same wire name.
pub(super) fn extend_schemas(set: &super::McpToolSet, schemas: &mut Vec<ToolSchema>) {
    for client in &set.clients {
        if !client.supports_resources() || set.is_disabled(client.name()) {
            continue;
        }
        for schema in [
            list_resources_schema(client.name()),
            read_resource_schema(client.name()),
        ] {
            if !shadowed_by_real_tool(set, &schema.name) {
                schemas.push(schema);
            }
        }
    }
}

/// Route `name` if it is some resources-capable server's synthetic tool:
/// drive the wire call and render its bounded, model-visible answer — or the
/// session-disabled error, matching how a real server's tools answer while
/// disabled. `None` for every other name, including a wire name a real tool
/// claims ([`shadowed_by_real_tool`] — advertise ⇔ answer).
pub(super) async fn route(
    set: &super::McpToolSet,
    name: &str,
    input: &Value,
) -> Option<ToolOutput> {
    if shadowed_by_real_tool(set, name) {
        return None;
    }
    for client in &set.clients {
        if !client.supports_resources() {
            continue;
        }
        let server = client.name();
        let is_list = name == list_resources_tool_name(server);
        if !is_list && name != read_resource_tool_name(server) {
            continue;
        }
        if set.is_disabled(server) {
            return Some(ToolOutput::Error {
                message: format!(
                    "mcp server `{server}` is disabled for this session — tool `{name}` unavailable"
                ),
            });
        }
        let output = if is_list {
            run_list(client, input).await
        } else {
            run_read(client, input).await
        };
        if let (Some(ledger), ToolOutput::Ok { .. }) = (&set.usage, &output) {
            // Record the successful call for the `mcp_usage` telemetry table,
            // exactly like a routed server tool (`execute_mcp`): these calls
            // spend the same server round trips.
            let raw_tool = if is_list {
                LIST_RESOURCES_TOOL
            } else {
                READ_RESOURCE_TOOL
            };
            let reason = input
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            push_usage(ledger, McpUsageRecord::now(server, raw_tool, reason));
        }
        return Some(output);
    }
    None
}

/// Whether `wire` is already claimed by a server-advertised tool: routed (the
/// real tool wins — a server that ships its own `list_resources` keeps it) or
/// contested (#2675 dropped every claimant, and a synthetic answering an
/// ambiguous name would quietly pick a winner after all).
fn shadowed_by_real_tool(set: &super::McpToolSet, wire: &str) -> bool {
    set.routes.contains_key(wire) || set.collisions.iter().any(|c| c.wire_name == wire)
}

/// The `list_resources` schema for `server`.
pub(super) fn list_resources_schema(server: &str) -> ToolSchema {
    ToolSchema {
        name: list_resources_tool_name(server),
        description: format!(
            "List the resources MCP server `{server}` exposes — files, documents, and other \
             data it offers as context. Returns one page of entries (URI, name, type, size); \
             when the result names a next-page cursor, pass it as `cursor` to fetch the \
             following page. Read a listed entry's contents with `{read}`.",
            read = read_resource_tool_name(server)
        ),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "cursor": {
                    "type": "string",
                    "description": "Opaque pagination cursor from a previous page's result. Omit for the first page."
                }
            }
        }),
        // Read-only (advertisable on read-only surfaces): `resources/list` is
        // the protocol's read surface and this schema is authored here, not by
        // the untrusted server. Never speculated: the server's request budget
        // and rate limit are not ours to spend twice (#923).
        read_only: true,
        speculation_safe: false,
    }
}

/// The `read_resource` schema for `server`.
pub(super) fn read_resource_schema(server: &str) -> ToolSchema {
    ToolSchema {
        name: read_resource_tool_name(server),
        description: format!(
            "Read one resource from MCP server `{server}` by its URI, exactly as listed by \
             `{list}`. Text contents are returned inline (truncated middle-out past the \
             standard result budget); binary contents are summarized, never inlined.",
            list = list_resources_tool_name(server)
        ),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "uri": {
                    "type": "string",
                    "description": "The resource URI, exactly as the listing returned it."
                }
            },
            "required": ["uri"]
        }),
        // Same posture as `list_resources` above, for the same reasons.
        read_only: true,
        speculation_safe: false,
    }
}

/// Drive one `resources/list` page and render it.
async fn run_list(client: &McpClient, input: &Value) -> ToolOutput {
    let cursor = match input.get("cursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(cursor)) => Some(cursor.as_str()),
        Some(other) => {
            return ToolOutput::Error {
                message: format!("argument `cursor` must be a string, got: {other}"),
            };
        }
    };
    match client.list_resources(cursor).await {
        Ok(page) => ToolOutput::Ok {
            content: bounded(render_listing(client.name(), &page)),
        },
        Err(err) => ToolOutput::Error {
            message: format!(
                "mcp server `{}` failed listing resources: {}",
                client.name(),
                err.user_message()
            ),
        },
    }
}

/// Drive one `resources/read` and render its contents.
async fn run_read(client: &McpClient, input: &Value) -> ToolOutput {
    let uri = match input.get("uri") {
        Some(Value::String(uri)) if !uri.is_empty() => uri.as_str(),
        _ => {
            return ToolOutput::Error {
                message: format!(
                    "tool `{}` requires a string `uri` argument naming the resource to read",
                    read_resource_tool_name(client.name())
                ),
            };
        }
    };
    match client.read_resource(uri).await {
        Ok(read) => ToolOutput::Ok {
            content: bounded(render_read(client.name(), uri, &read)),
        },
        Err(err) => ToolOutput::Error {
            message: format!(
                "mcp server `{}` failed reading resource `{uri}`: {}",
                client.name(),
                err.user_message()
            ),
        },
    }
}

/// Cap a rendered payload at the same per-result ingest budget every
/// `tools/call` result gets (#551) — resource payloads are untrusted input
/// arriving through a second door, not a budget exemption.
fn bounded(rendered: String) -> String {
    if rendered.len() > MAX_TOOL_RESULT_BYTES {
        truncate_middle_out(&rendered, MAX_TOOL_RESULT_BYTES)
    } else {
        rendered
    }
}

/// Render one listing page: a header, one line per resource, and — when the
/// server named a continuation cursor — an explicit next-page instruction.
fn render_listing(server: &str, page: &ListResourcesResult) -> String {
    let mut out = if page.resources.is_empty() {
        format!("mcp server `{server}` lists no resources on this page")
    } else {
        let mut lines = Vec::with_capacity(page.resources.len() + 1);
        lines.push(format!(
            "resources on mcp server `{server}` (read one with `{read}`):",
            read = read_resource_tool_name(server)
        ));
        for resource in &page.resources {
            lines.push(render_listing_line(resource));
        }
        lines.join("\n")
    };
    if let Some(cursor) = page.next_cursor.as_deref().filter(|c| !c.is_empty()) {
        out.push_str(&format!(
            "\n[more resources: call again with cursor `{}`]",
            flatten(cursor)
        ));
    }
    out
}

/// One `- <uri> — <name> (<type>, <size>): <description>` listing line, every
/// field newline-flattened so one (untrusted) resource is always one line.
fn render_listing_line(resource: &ResourceInfo) -> String {
    let mut line = format!("- {}", flatten(&resource.uri));
    if !resource.name.is_empty() && resource.name != resource.uri {
        line.push_str(&format!(" — {}", flatten(&resource.name)));
    }
    let mut meta: Vec<String> = Vec::new();
    if let Some(mime) = resource.mime_type.as_deref().filter(|m| !m.is_empty()) {
        meta.push(flatten(mime));
    }
    if let Some(size) = resource.size {
        meta.push(format!("{size} bytes"));
    }
    if !meta.is_empty() {
        line.push_str(&format!(" ({})", meta.join(", ")));
    }
    if let Some(description) = resource
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        line.push_str(&format!(
            ": {}",
            truncate(&flatten(description), MAX_LIST_DESCRIPTION_CHARS)
        ));
    }
    line
}

/// Render a read's contents through the same door as an embedded resource in
/// a `tools/call` result ([`render_resource_contents`]) — text inline, blobs
/// summarized — so the two ways a resource reaches the model read alike.
fn render_read(server: &str, uri: &str, read: &ReadResourceResult) -> String {
    if read.contents.is_empty() {
        return format!("resource `{uri}` on mcp server `{server}` returned no contents");
    }
    let mut out = String::new();
    for contents in &read.contents {
        let piece = render_resource_contents(contents);
        if piece.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&piece);
    }
    out
}

/// Collapse newlines so an untrusted field cannot fake extra listing lines.
fn flatten(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::super::{DisabledServers, McpToolSet};
    use super::*;
    use crate::client::ingest::MAX_TOOL_DESCRIPTION_CHARS;
    use crate::protocol::PREFERRED_PROTOCOL_VERSION;
    use crate::transport::testkit::ScriptedTransport;
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};
    use stella_core::ports::ToolExecutor as _;

    /// A connected client whose server declared the `resources` capability
    /// and advertises `tools`, with `resource_responses` pre-queued (the
    /// scripted transport cannot be pushed to once boxed).
    async fn resources_client(
        name: &str,
        tools: serde_json::Value,
        resource_responses: &[(&str, serde_json::Value)],
    ) -> McpClient {
        let transport = ScriptedTransport::new();
        transport.push_ok(
            "initialize",
            serde_json::json!({
                "protocolVersion": PREFERRED_PROTOCOL_VERSION,
                "capabilities": { "tools": {}, "resources": {} }
            }),
        );
        transport.push_ok("tools/list", serde_json::json!({ "tools": tools }));
        for (method, value) in resource_responses {
            transport.push_ok(method, value.clone());
        }
        let mut client = McpClient::new(name, Box::new(transport));
        client.initialize().await.unwrap();
        client
    }

    /// A connected client whose server declared NO capabilities at all.
    async fn plain_client(name: &str) -> McpClient {
        let transport = ScriptedTransport::new();
        transport.push_ok(
            "initialize",
            serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
        );
        transport.push_ok(
            "tools/list",
            serde_json::json!({ "tools": [{ "name": "echo", "inputSchema": { "type": "object" } }] }),
        );
        let mut client = McpClient::new(name, Box::new(transport));
        client.initialize().await.unwrap();
        client
    }

    #[test]
    fn the_synthetic_names_follow_the_mcp_namespace_convention() {
        assert_eq!(
            list_resources_tool_name("docs"),
            "mcp__docs__list_resources"
        );
        assert_eq!(read_resource_tool_name("docs"), "mcp__docs__read_resource");
    }

    #[test]
    fn the_schemas_are_read_only_bounded_and_deterministic() {
        for schema in [list_resources_schema("docs"), read_resource_schema("docs")] {
            assert!(schema.read_only, "the resource verbs are the read surface");
            assert!(
                !schema.speculation_safe,
                "server request budgets are not ours to spend twice"
            );
            assert!(
                schema.description.chars().count() <= MAX_TOOL_DESCRIPTION_CHARS,
                "synthetic tools honor the same ingest budget as real ones"
            );
        }
        // Deterministic: the advertised surface is byte-stable per server.
        assert_eq!(list_resources_schema("docs"), list_resources_schema("docs"));
        assert_eq!(read_resource_schema("docs"), read_resource_schema("docs"));
        // `list` is callable with `{}`; `read` genuinely requires its uri.
        assert!(super::super::accepts_empty_input(
            &list_resources_schema("docs").input_schema
        ));
        assert!(!super::super::accepts_empty_input(
            &read_resource_schema("docs").input_schema
        ));
    }

    #[tokio::test]
    async fn only_a_resources_capable_server_advertises_the_two_tools() {
        let capable = resources_client("docs", serde_json::json!([]), &[]).await;
        let plain = plain_client("plain").await;
        let set = McpToolSet::from_clients(vec![capable, plain]);

        let names: Vec<String> = set.schemas().into_iter().map(|s| s.name).collect();
        assert!(
            names.contains(&"mcp__docs__list_resources".to_string()),
            "{names:?}"
        );
        assert!(
            names.contains(&"mcp__docs__read_resource".to_string()),
            "{names:?}"
        );
        assert!(
            !names
                .iter()
                .any(|n| n.starts_with("mcp__plain__list_resources")
                    || n.starts_with("mcp__plain__read_resource")),
            "a server that never declared the capability is not probed for it: {names:?}"
        );

        // And calling the undeclared one is a model-visible unknown-tool miss.
        let out = set
            .execute("mcp__plain__list_resources", &serde_json::Value::Null)
            .await;
        match out {
            ToolOutput::Error { message } => {
                assert!(message.contains("not advertised"), "{message}");
            }
            other => panic!("expected the unknown-tool error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_resources_renders_the_page_and_names_the_next_cursor() {
        let client = resources_client(
            "docs",
            serde_json::json!([]),
            &[(
                "resources/list",
                serde_json::json!({
                    "resources": [
                        {
                            "uri": "file:///guide.md",
                            "name": "guide.md",
                            "mimeType": "text/markdown",
                            "size": 1234,
                            "description": "the\nuser guide"
                        },
                        { "uri": "file:///logo.png" }
                    ],
                    "nextCursor": "p2"
                }),
            )],
        )
        .await;
        let set = McpToolSet::from_clients(vec![client]);

        match set
            .execute("mcp__docs__list_resources", &serde_json::Value::Null)
            .await
        {
            ToolOutput::Ok { content } => {
                assert!(
                    content.contains(
                        "- file:///guide.md — guide.md (text/markdown, 1234 bytes): the user guide"
                    ),
                    "one flattened line per resource: {content}"
                );
                assert!(content.contains("- file:///logo.png"), "{content}");
                assert!(
                    content.contains("call again with cursor `p2`"),
                    "the continuation is an explicit instruction: {content}"
                );
                assert!(
                    content.contains("`mcp__docs__read_resource`"),
                    "the listing names the read tool: {content}"
                );
            }
            other => panic!("expected the listing, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_resource_inlines_text_and_summarizes_a_blob() {
        let client = resources_client(
            "docs",
            serde_json::json!([]),
            &[
                (
                    "resources/read",
                    serde_json::json!({
                        "contents": [{ "uri": "file:///a.txt", "mimeType": "text/plain", "text": "hello" }]
                    }),
                ),
                (
                    "resources/read",
                    serde_json::json!({
                        "contents": [{ "uri": "file:///b.png", "mimeType": "image/png", "blob": "QUFBQQ==" }]
                    }),
                ),
            ],
        )
        .await;
        let set = McpToolSet::from_clients(vec![client]);

        match set
            .execute(
                "mcp__docs__read_resource",
                &serde_json::json!({ "uri": "file:///a.txt" }),
            )
            .await
        {
            ToolOutput::Ok { content } => {
                assert_eq!(content, "[resource: file:///a.txt]\nhello");
            }
            other => panic!("expected the text inline, got {other:?}"),
        }

        match set
            .execute(
                "mcp__docs__read_resource",
                &serde_json::json!({ "uri": "file:///b.png" }),
            )
            .await
        {
            ToolOutput::Ok { content } => {
                assert_eq!(
                    content,
                    "[resource: file:///b.png — binary image/png, not inlined]"
                );
                assert!(!content.contains("QUFBQQ=="), "base64 never inlines");
            }
            other => panic!("expected the blob summary, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_resource_requires_a_string_uri() {
        let client = resources_client("docs", serde_json::json!([]), &[]).await;
        let set = McpToolSet::from_clients(vec![client]);
        for input in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({ "uri": 7 }),
            serde_json::json!({ "uri": "" }),
        ] {
            match set.execute("mcp__docs__read_resource", &input).await {
                ToolOutput::Error { message } => {
                    assert!(message.contains("`uri`"), "names the argument: {message}");
                }
                other => panic!("expected the missing-uri error for {input}, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_disabled_server_hides_and_refuses_its_resource_tools() {
        let client = resources_client("docs", serde_json::json!([]), &[]).await;
        let disabled: DisabledServers = Arc::new(Mutex::new(HashSet::new()));
        disabled.lock().unwrap().insert("docs".to_string());
        let set = McpToolSet::from_clients(vec![client]).with_disabled_servers(disabled.clone());

        assert!(
            set.schemas().is_empty(),
            "a switched-off server advertises nothing, resource tools included"
        );
        match set
            .execute("mcp__docs__list_resources", &serde_json::Value::Null)
            .await
        {
            ToolOutput::Error { message } => assert!(message.contains("disabled"), "{message}"),
            other => panic!("expected the disabled error, got {other:?}"),
        }

        // Re-enabling brings both back, live.
        disabled.lock().unwrap().clear();
        assert_eq!(set.schemas().len(), 2);
    }

    #[tokio::test]
    async fn an_oversized_resource_read_is_truncated_under_the_result_budget() {
        let client = resources_client(
            "docs",
            serde_json::json!([]),
            &[(
                "resources/read",
                serde_json::json!({
                    "contents": [{
                        "uri": "file:///big.txt",
                        "text": format!("HEAD{}TAIL", "x".repeat(MAX_TOOL_RESULT_BYTES * 2))
                    }]
                }),
            )],
        )
        .await;
        let set = McpToolSet::from_clients(vec![client]);

        match set
            .execute(
                "mcp__docs__read_resource",
                &serde_json::json!({ "uri": "file:///big.txt" }),
            )
            .await
        {
            ToolOutput::Ok { content } => {
                assert!(
                    content.len() < MAX_TOOL_RESULT_BYTES + 128,
                    "kept text respects the budget: {} bytes",
                    content.len()
                );
                assert!(content.contains("... [truncated "), "elision is explicit");
                assert!(content.ends_with("TAIL"), "the tail survives (L-S3)");
            }
            other => panic!("expected the capped read, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_real_tool_wearing_the_wire_name_shadows_the_synthetic() {
        // The server's OWN `list_resources` tool keeps its name; the synthetic
        // stands aside instead of advertising a duplicate. `read_resource`
        // is uncontested and still rides.
        let client = resources_client(
            "docs",
            serde_json::json!([{ "name": "list_resources", "inputSchema": { "type": "object" } }]),
            &[],
        )
        .await;
        let set = McpToolSet::from_clients(vec![client]);

        let schemas = set.schemas();
        let list: Vec<&ToolSchema> = schemas
            .iter()
            .filter(|s| s.name == "mcp__docs__list_resources")
            .collect();
        assert_eq!(list.len(), 1, "exactly one claimant survives");
        assert!(
            !list[0].read_only,
            "and it is the server's own (untrusted ⇒ mutating) tool, not the synthetic"
        );
        assert!(
            schemas.iter().any(|s| s.name == "mcp__docs__read_resource"),
            "the uncontested synthetic still rides"
        );
    }

    #[tokio::test]
    async fn the_resource_tools_are_withheld_from_best_of_n_candidates() {
        let client = resources_client("docs", serde_json::json!([]), &[]).await;
        let set =
            Arc::new(McpToolSet::from_clients(vec![client]).with_candidate_safe_servers(["docs"]));
        assert!(
            !set.is_candidate_safe_tool("mcp__docs__list_resources"),
            "synthetic tools never enter the routing map, so candidates never see them"
        );
        assert!(!set.is_candidate_safe_tool("mcp__docs__read_resource"));
    }

    #[tokio::test]
    async fn a_successful_resource_call_is_recorded_in_the_usage_ledger() {
        use stella_core::mcp_usage::drain_usage;
        let client = resources_client(
            "docs",
            serde_json::json!([]),
            &[("resources/list", serde_json::json!({ "resources": [] }))],
        )
        .await;
        let ledger: stella_core::mcp_usage::McpUsageLedger = Arc::new(Mutex::new(Vec::new()));
        let set = McpToolSet::from_clients(vec![client]).with_usage_ledger(ledger.clone());

        let out = set
            .execute("mcp__docs__list_resources", &serde_json::Value::Null)
            .await;
        assert!(matches!(out, ToolOutput::Ok { .. }));

        let records = drain_usage(&ledger);
        assert_eq!(records.len(), 1, "one wire round trip, one record");
        assert_eq!(records[0].server, "docs");
        assert_eq!(records[0].tool, "list_resources");
    }
}
