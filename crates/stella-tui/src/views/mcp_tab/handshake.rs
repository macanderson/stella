// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The MCP tab's first-enable handshake (SPEC §9.3): what a server declares,
//! and the grant it cannot be used without.
//!
//! A **mode**, not an overlay — the answer is required before the server is
//! usable at all, and an overlay reads as something you can dismiss.
//!
//! Every declared tool is rendered by name and the pane scrolls, because a
//! capability grant whose subject is "14 tools" is a grant nobody read. Denying
//! costs no keystroke: withheld is the default, so walking away is the answer.
//!
//! Nothing here withholds anything. `stella_mcp::CapabilityGrants` is what the
//! tool layer consults and `stella_mcp::McpServerEntry::granted` is where the
//! answer persists; this module is the question.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use stella_tui_theme::token;

use super::McpTabState;
use crate::envelope::McpServerDetail;

/// The first-enable handshake gate: one server's declared capabilities, and
/// the question.
///
/// SPEC §9.3 promises "the handshake shows capabilities before first enable",
/// and this is the surface that keeps it. It reuses [`McpServerDetail`] rather
/// than a shape of its own because the operator is being asked about exactly
/// what the inspector shows — what the server announced at handshake and every
/// tool it advertises — and a second, thinner summary of the same facts is how
/// the two drift into disagreeing about what was granted.
///
/// A mode rather than an overlay: the answer is required before the server is
/// usable at all, and an overlay reads as something you can dismiss.
#[derive(Debug, Clone)]
pub struct HandshakeGate {
    /// The alias being reviewed. Kept before `detail` arrives so a late reply
    /// for another server cannot repaint this one.
    pub server: String,
    /// The assembled detail; `None` renders a loading state, and the grant key
    /// stands down until it lands — nobody can grant capabilities they have
    /// not been shown.
    pub detail: Option<McpServerDetail>,
    /// Vertical scroll offset in lines, clamped to content at render time. A
    /// server with forty tools must not be grantable by a reader who could
    /// only ever see the first six.
    pub scroll: u16,
}

/// Paint the gate: what this server declares, then what granting costs.
///
/// Who the process on the other end says it is, then every tool it wants the
/// model to be able to call, then what withholding costs — the order the
/// question is answered in. The tool list is the substance (a capability grant
/// whose subject is a count is a grant nobody read), so it is rendered in full
/// and scrolled rather than truncated.
///
/// `height` is the pane's, used only to say how much is left to read; the
/// scroll itself is [`crate::deck_ui::list_nav`]'s business in the key handler.
pub(super) fn render(state: &McpTabState, lines: &mut Vec<Line<'static>>, height: usize) {
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    let Some(gate) = &state.handshake else {
        return;
    };
    lines.push(Line::from(vec![
        Span::styled("  handshake ", Style::new().fg(token::GOLD)),
        Span::styled(gate.server.clone(), text),
        Span::styled("  ·  first enable", muted),
    ]));
    lines.push(Line::default());

    let Some(detail) = &gate.detail else {
        lines.push(Line::from(Span::styled(
            "  reading what the server declares…",
            muted,
        )));
        return;
    };

    let mut body: Vec<Line<'static>> = Vec::new();

    // Who answered the pipe. The alias is a local nickname; this is the only
    // identity the process itself offered, and a disagreement between the two
    // is exactly what a reviewer is looking for.
    let announced = detail
        .live
        .as_ref()
        .and_then(|l| {
            [l.title.as_deref(), l.name.as_deref()]
                .into_iter()
                .flatten()
                .find(|s| !s.trim().is_empty())
        })
        .unwrap_or("(the server announced no name)");
    body.push(Line::from(vec![
        Span::styled("  announces itself as  ", muted),
        Span::styled(announced.to_string(), text),
    ]));
    body.push(Line::from(vec![
        Span::styled("  reached at           ", muted),
        Span::styled(detail.endpoint.clone(), text),
    ]));
    if let Some(live) = &detail.live
        && !live.protocol_version.is_empty()
    {
        body.push(Line::from(vec![
            Span::styled("  speaking             ", muted),
            Span::styled(live.protocol_version.clone(), text),
        ]));
    }
    if !detail.auth_fields.is_empty() {
        body.push(Line::from(vec![
            Span::styled("  sending credentials  ", muted),
            Span::styled(detail.auth_fields.join(", "), text),
        ]));
    }
    body.push(Line::default());

    // The capabilities themselves.
    if detail.tools.is_empty() {
        body.push(Line::from(Span::styled(
            if detail.connected {
                "  it declares no tools at all"
            } else {
                "  it is not connected, so it has declared nothing to grant"
            },
            Style::new().fg(token::RED),
        )));
    } else {
        body.push(Line::from(vec![
            Span::styled("  it declares ", muted),
            Span::styled(detail.tools.len().to_string(), text),
            Span::styled(
                if detail.tools.len() == 1 {
                    " tool, which the model may call once granted:"
                } else {
                    " tools, every one of which the model may call once granted:"
                },
                muted,
            ),
        ]));
    }
    for tool in &detail.tools {
        let mut spans = vec![
            Span::raw("    "),
            Span::styled(tool.name.clone(), text.add_modifier(Modifier::BOLD)),
        ];
        if tool.safe_to_retry {
            // The server's own claim about its own tool, and worth exactly
            // that much — the word is here so the reviewer weighs it, not so
            // they trust it.
            spans.push(Span::styled("  read-only (self-reported)", dim));
        }
        body.push(Line::from(spans));
        let summary = tool
            .description
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or_default();
        if !summary.is_empty() {
            body.push(Line::from(Span::styled(format!("      {summary}"), dim)));
        }
    }

    body.push(Line::default());
    body.push(Line::from(Span::styled(
        "  granting records the decision in .stella/mcp.toml · until then no tool of this \
         server is offered to the model and every call to it is refused",
        dim,
    )));

    // Scroll the capability list rather than truncating it: a grant whose
    // subject is a count ("14 tools") is a grant nobody read, and a forty-tool
    // server must not be grantable by a reader who could only see six.
    let scroll = usize::from(gate.scroll).min(body.len().saturating_sub(1));
    let room = height.saturating_sub(lines.len());
    let more = body.len().saturating_sub(scroll) > room;
    lines.extend(body.into_iter().skip(scroll));
    if more {
        lines.push(Line::from(Span::styled("  ↑↓ more to read", muted)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::mcp_tab::{McpMode, footer};

    fn flat(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn rendered(state: &McpTabState, height: usize) -> String {
        let mut lines = Vec::new();
        render(state, &mut lines, height);
        lines.iter().map(flat).collect::<Vec<_>>().join("\n")
    }

    /// **The witness (#5047, handshake).** The caption promised "the handshake
    /// shows capabilities before first enable" and nothing showed them: the
    /// declared tool list was reachable only afterwards, in the ctrl+o
    /// inspector. The gate renders every declared tool by name, and says what
    /// withholding costs.
    #[test]
    fn the_first_enable_handshake_names_every_declared_capability() {
        let state = McpTabState {
            mode: McpMode::Handshake,
            handshake: Some(HandshakeGate {
                server: "mcp".into(),
                detail: Some(McpServerDetail {
                    name: "mcp".into(),
                    endpoint: "https://mcp.stripe.com/v1".into(),
                    connected: true,
                    live: Some(crate::envelope::McpLiveIdentity {
                        name: Some("stripe-mcp".into()),
                        protocol_version: "2025-06-18".into(),
                        ..crate::envelope::McpLiveIdentity::default()
                    }),
                    auth_fields: vec!["Authorization".into()],
                    tools: vec![
                        crate::envelope::McpToolRow {
                            name: "create_refund".into(),
                            description: "Refund a charge.".into(),
                            ..crate::envelope::McpToolRow::default()
                        },
                        crate::envelope::McpToolRow {
                            name: "list_charges".into(),
                            description: "List charges.".into(),
                            safe_to_retry: true,
                            ..crate::envelope::McpToolRow::default()
                        },
                    ],
                    ..McpServerDetail::default()
                }),
                scroll: 0,
            }),
            ..McpTabState::default()
        };
        let text = rendered(&state, 40);

        assert!(text.contains("stripe-mcp"), "who answered: {text}");
        assert!(text.contains("mcp.stripe.com"), "where: {text}");
        assert!(text.contains("2025-06-18"), "protocol: {text}");
        assert!(text.contains("Authorization"), "credentials sent: {text}");
        // Every declared tool by name — a grant whose subject is a count is a
        // grant nobody read.
        assert!(text.contains("create_refund"), "{text}");
        assert!(text.contains("list_charges"), "{text}");
        assert!(
            text.contains("self-reported"),
            "a server's read-only claim is its own: {text}"
        );
        assert!(
            text.contains("every call to it is refused"),
            "the gate says what withholding costs: {text}"
        );
        assert!(flat(&footer(McpMode::Handshake)).contains("g grant"));
    }

    /// The gate cannot be answered before it has anything to show. A "grant"
    /// keystroke over a loading pane would be a decision about capabilities
    /// nobody was shown.
    #[test]
    fn the_handshake_says_it_is_still_reading_before_the_detail_lands() {
        let state = McpTabState {
            mode: McpMode::Handshake,
            handshake: Some(HandshakeGate {
                server: "mcp".into(),
                detail: None,
                scroll: 0,
            }),
            ..McpTabState::default()
        };
        let text = rendered(&state, 40);
        assert!(text.contains("reading what the server declares"), "{text}");
        assert!(!text.contains("declares 0"), "{text}");
    }

    /// A late detail addressed to another server never repaints the open gate,
    /// for the same reason it never repaints the inspector: every field on
    /// screen would silently be mislabelled.
    #[test]
    fn a_late_detail_for_another_server_never_fills_the_open_gate() {
        let mut state = McpTabState {
            mode: McpMode::Handshake,
            handshake: Some(HandshakeGate {
                server: "fs".into(),
                detail: None,
                scroll: 0,
            }),
            ..McpTabState::default()
        };
        state.apply_detail(McpServerDetail {
            name: "mcp".into(),
            ..McpServerDetail::default()
        });
        assert!(state.handshake.as_ref().unwrap().detail.is_none());
        state.apply_detail(McpServerDetail {
            name: "fs".into(),
            ..McpServerDetail::default()
        });
        assert!(state.handshake.unwrap().detail.is_some());
    }
}
