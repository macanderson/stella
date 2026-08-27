// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The MCP tab's golden fixture: four configured servers, chosen so the
//! golden pins every claim SPEC §9.3 makes about the list.
//!
//! A submodule of `deck_render_snapshots` rather than more lines in it: the
//! parent had already reached the 1500-line ceiling, so this fixture goes
//! here instead of pushing it over.
//!
//! The parent renders the golden and owns the assertion, so the snapshot
//! lives in one directory and is blessed by one command:
//! `BLESS=1 cargo test -p stella-tui --test deck_render_snapshots`.

/// The MCP tab's servers, as the driver delivers them — which is the point of
/// the ordering below.
///
/// The four rows are not decoration. Between them they pin the whole of SPEC
/// §9.3's list: the graph server arrives *second* (`mcp.toml`'s keys are
/// alphabetical, so that is where it really lands) and the golden proves the
/// paint moves it to the top with its caption; every connected row carries a
/// measured latency; and `linear` is a server just installed from the
/// registry — connected, and useless until its handshake is reviewed, so the
/// golden proves the row says `ungranted` in words.
pub(super) fn fixture_mcp_servers() -> Vec<stella_tui::McpServerInfo> {
    vec![
        stella_tui::McpServerInfo {
            name: "stripe".into(),
            title: Some("Stripe".into()),
            description: Some("Payments, refunds, and balance reads.".into()),
            endpoint: "https://mcp.stripe.com/v1".into(),
            kind: "http".into(),
            enabled: true,
            connected: true,
            health: Some("live".into()),
            tool_count: 21,
            oauth: Some(true),
            calls: 14,
            latency_ms: Some(66),
            granted: true,
            ..Default::default()
        },
        stella_tui::McpServerInfo {
            name: stella_tui::GRAPH_SERVER.into(),
            description: Some("stella's own code graph — nodes, edges, coupling.".into()),
            endpoint: "stella-graph-mcp".into(),
            kind: "stdio".into(),
            enabled: true,
            connected: true,
            health: Some("live".into()),
            tool_count: 14,
            latency_ms: Some(8),
            granted: true,
            ..Default::default()
        },
        stella_tui::McpServerInfo {
            name: "fs".into(),
            endpoint: "npx -y @modelcontextprotocol/server-filesystem /w".into(),
            kind: "stdio".into(),
            enabled: true,
            connected: true,
            health: Some("live".into()),
            tool_count: 4,
            latency_ms: Some(2),
            granted: true,
            ..Default::default()
        },
        stella_tui::McpServerInfo {
            name: "linear".into(),
            title: Some("Linear".into()),
            description: Some("Issues, projects, cycles, and documents.".into()),
            endpoint: "https://mcp.linear.app/mcp".into(),
            kind: "http".into(),
            enabled: true,
            connected: true,
            health: Some("live".into()),
            tool_count: 9,
            latency_ms: Some(41),
            oauth: Some(false),
            granted: false,
            ..Default::default()
        },
    ]
}
