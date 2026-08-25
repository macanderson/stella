// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The MCP tab's ctrl+o inspector: everything known about one configured
//! server, on one scrollable surface.
//!
//! ```text
//! ╭ Stripe ─────────────────────────────── ↑↓ scroll · r look up · esc close ╮
//! │ mcp  ·  http  ·  live                                                    │
//! │   Payments, refunds, and balance reads.                                  │
//! │   where ─────────────────────────────────────────────────────────────────│
//! │     endpoint     https://mcp.stripe.com/v1                               │
//! ╰──────────────────────────────────────────────────────────────────────────╯
//! ```
//!
//! Four bands, in the order an operator asks the questions:
//!
//! 1. **What is it** — the recorded description or the server's own handshake
//!    instructions, plus the registry name, repository, and version.
//! 2. **Where is it** — transport, endpoint (query values redacted), and what
//!    the live process announced itself as.
//! 3. **How does it authenticate** — configured credential *field names* and
//!    OAuth state. Never a value.
//! 4. **What can it do** — every advertised tool with its description and call
//!    count, which is what answers "what is this server for" when no prose
//!    exists anywhere.
//!
//! A floating surface, so it draws its own frame — the one border a tab is
//! allowed, and the same rounded [`stella_tui_theme::token::BORDER`] block
//! every other overlay carries ([`crate::views::sessions`]).

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget, Wrap};
use stella_tui_theme::token;

use super::truncate;
use crate::deck_ui::DeckUi;
use crate::envelope::{McpLookupState, McpServerDetail};

/// Label column width, so every `key   value` pair lines up.
const LABEL: usize = 13;

/// Draw the inspector popup over `area`.
pub fn render(ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    let Some(inspector) = ui.mcp.inspector.as_mut() else {
        return;
    };
    let w = area.width.saturating_sub(4).clamp(24, 104);
    let h = area.height.saturating_sub(2).clamp(6, 36);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    Clear.render(rect, buf);

    let heading = inspector
        .detail
        .as_ref()
        .map(McpServerDetail::display_name)
        .unwrap_or(inspector.server.as_str());
    let hint = match inspector.detail.as_ref() {
        // `r` is offered only when it could answer something — see
        // `McpServerDetail::lookup_would_help`.
        Some(d) if d.lookup_would_help() => " ↑↓ scroll · r look up · esc close ",
        _ => " ↑↓ scroll · esc close ",
    };
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            truncate(heading, (w as usize).saturating_sub(hint.len() + 6).max(8)),
            Style::new().fg(token::TEXT),
        ),
        Span::raw(" "),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(token::BORDER))
        .title(title)
        .title(Line::from(Span::styled(hint, Style::new().fg(token::DIM))).right_aligned());
    let inner = block.inner(rect);
    block.render(rect, buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let Some(detail) = inspector.detail.clone() else {
        Paragraph::new("gathering server detail…")
            .style(Style::new().fg(token::MUTED))
            .alignment(Alignment::Center)
            .render(centered_row(inner), buf);
        return;
    };

    // A pinned subtitle (alias + status), then the scrollable body — so the
    // operator never scrolls past the answer to "which server is this".
    let bands = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
    Paragraph::new(subtitle(&detail)).render(bands[0], buf);

    let body = Text::from(body_lines(&detail, bands[1].width as usize));
    let max_scroll = body.height().saturating_sub(bands[1].height as usize) as u16;
    let scroll = inspector.scroll.min(max_scroll);
    inspector.scroll = scroll;
    Paragraph::new(body)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
        .render(bands[1], buf);
}

/// The pinned identity line: alias, transport, and live status.
///
/// The status takes the same three colours the list row gives it — dim when
/// disabled, green when live, red when it is configured and not answering —
/// so an operator who opened the inspector from a red row does not find the
/// same fact in a different metal.
fn subtitle(detail: &McpServerDetail) -> Line<'static> {
    let muted = Style::new().fg(token::MUTED);
    let status = if !detail.enabled {
        Span::styled("disabled", Style::new().fg(token::DIM))
    } else if detail.connected {
        Span::styled(
            detail.health.clone().unwrap_or_else(|| "live".to_string()),
            Style::new().fg(token::GREEN),
        )
    } else {
        Span::styled("not connected", Style::new().fg(token::RED))
    };
    Line::from(vec![
        Span::raw(" "),
        Span::styled(detail.name.clone(), Style::new().fg(token::TEXT)),
        Span::styled(format!("  ·  {}  ·  ", detail.kind), muted),
        status,
    ])
}

fn body_lines(detail: &McpServerDetail, width: usize) -> Vec<Line<'static>> {
    let muted = Style::new().fg(token::MUTED);
    let mut lines = Vec::new();

    // ── What is it ──────────────────────────────────────────────────────────
    if let Some(desc) = detail
        .description
        .as_deref()
        .filter(|d| !d.trim().is_empty())
    {
        lines.push(Line::default());
        for line in wrap(desc.trim(), width.saturating_sub(2)) {
            lines.push(Line::from(Span::styled(
                format!("  {line}"),
                Style::new().fg(token::TEXT),
            )));
        }
    }
    lines.push(Line::default());
    lines.extend(lookup_note(&detail.lookup));

    // ── Where is it ─────────────────────────────────────────────────────────
    lines.push(section("where", width));
    lines.push(field("endpoint", &detail.endpoint));
    if let Some(name) = &detail.registry_name {
        lines.push(field("registry", name));
    }
    if let Some(repo) = &detail.repository {
        lines.push(field("repository", repo));
    }
    if let Some(version) = &detail.version {
        lines.push(field("version", version));
    }

    // What the process on the other end claims, which can differ from the card
    // — worth showing side by side rather than merged.
    if let Some(live) = &detail.live {
        let announced = [live.title.as_deref(), live.name.as_deref()]
            .into_iter()
            .flatten()
            .find(|s| !s.trim().is_empty());
        if let Some(announced) = announced {
            let version = live
                .version
                .as_deref()
                .filter(|v| !v.is_empty())
                .map(|v| format!(" v{v}"))
                .unwrap_or_default();
            lines.push(field("announced", &format!("{announced}{version}")));
        }
        if let Some(site) = &live.website_url {
            lines.push(field("website", site));
        }
        if !live.protocol_version.is_empty() {
            lines.push(field("protocol", &live.protocol_version));
        }
    }

    // ── How does it authenticate ────────────────────────────────────────────
    lines.push(Line::default());
    lines.push(section("auth", width));
    if detail.auth_fields.is_empty() {
        lines.push(field("credentials", "none configured"));
    } else {
        // Names only. The values live in `mcp.toml` and are never rendered.
        lines.push(field("credentials", &detail.auth_fields.join(", ")));
    }
    match detail.oauth {
        Some(true) => lines.push(field("oauth", "logged in — tokens auto-refresh")),
        Some(false) => lines.push(field("oauth", "not logged in (o on the list to log in)")),
        None => lines.push(field("oauth", "n/a — stdio server")),
    }
    if detail.candidate_safe {
        lines.push(field("candidates", "shared into Best-of-N workspaces"));
    }

    // ── What can it do ──────────────────────────────────────────────────────
    lines.push(Line::default());
    lines.push(section(&format!("tools ({})", detail.tools.len()), width));
    if detail.dropped_tools > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "    ! at least {} more advertised past the cap — never offered to the model",
                detail.dropped_tools
            ),
            Style::new().fg(token::RED),
        )));
    }
    // The other wall (#4441). Worded apart from the count cap above and
    // without "at least": the budget sees the whole advertised list, so this
    // number is exact where that one is a floor.
    if detail.trimmed_tools > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "    ! {} trimmed over the per-server schema byte budget — never offered to the model",
                detail.trimmed_tools
            ),
            Style::new().fg(token::RED),
        )));
    }
    if detail.tools.is_empty() {
        lines.push(Line::from(Span::styled(
            if detail.connected {
                "    this server advertises no tools"
            } else {
                "    not connected this session — its tool list is unknown"
            },
            muted,
        )));
    }
    for tool in &detail.tools {
        let mut spans = vec![
            Span::raw("    "),
            Span::styled(
                tool.name.clone(),
                Style::new().fg(token::TEXT).add_modifier(Modifier::BOLD),
            ),
        ];
        if tool.calls > 0 {
            spans.push(Span::styled(
                format!("  {}×", tool.calls),
                Style::new().fg(token::GOLD),
            ));
        }
        if tool.safe_to_retry {
            spans.push(Span::styled("  read-only", muted));
        }
        lines.push(Line::from(spans));
        // One line of the tool's own description — enough to tell tools apart,
        // and the full text is the model's business, not the operator's.
        let summary = tool
            .description
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or_default();
        if !summary.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("      {}", truncate(summary, width.saturating_sub(8))),
                Style::new().fg(token::DIM),
            )));
        }
    }

    // The server's own prose last: it is the longest and the least trustworthy
    // thing here, so it must not push the facts off the first screen.
    if let Some(instructions) = detail
        .live
        .as_ref()
        .and_then(|l| l.instructions.as_deref())
        .filter(|i| !i.trim().is_empty())
    {
        lines.push(Line::default());
        lines.push(section("instructions (the server's own words)", width));
        for line in wrap(instructions.trim(), width.saturating_sub(2)) {
            lines.push(Line::from(Span::styled(format!("  {line}"), muted)));
        }
    }

    lines
}

/// A note on the registry lookup, when one has been attempted.
fn lookup_note(state: &McpLookupState) -> Vec<Line<'static>> {
    let (text, style) = match state {
        McpLookupState::Idle => return Vec::new(),
        McpLookupState::Fetching => (
            "  looking this server up in the registry…".to_string(),
            Style::new().fg(token::MUTED),
        ),
        McpLookupState::Found => (
            "  description found and saved to .stella/mcp.toml".to_string(),
            Style::new().fg(token::GREEN),
        ),
        McpLookupState::Missing => (
            "  the registry has no entry under this name — nothing to describe it with".to_string(),
            Style::new().fg(token::MUTED),
        ),
        McpLookupState::Failed(error) => (
            format!("  registry lookup failed: {error}"),
            Style::new().fg(token::RED),
        ),
    };
    vec![Line::from(Span::styled(text, style)), Line::default()]
}

/// A section header: the label, then a dim rule out to `width`.
///
/// The rule is what separates a header from the `label   value` pairs under
/// it — without it the two indent levels alone have to carry the structure,
/// and a long field name reads as another heading.
fn section(label: &str, width: usize) -> Line<'static> {
    // Out to the full width, so every header's rule ends on the same column
    // — a ragged right edge would read as three unrelated decorations.
    let rule = width.saturating_sub(label.chars().count() + 4);
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            label.to_string(),
            Style::new().fg(token::MUTED).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {}", "─".repeat(rule)),
            Style::new().fg(token::RULE),
        ),
    ])
}

/// One `label   value` pair in the aligned key column.
fn field(label: &str, value: &str) -> Line<'static> {
    let pad = LABEL.saturating_sub(label.chars().count());
    Line::from(vec![
        Span::raw("    "),
        Span::styled(label.to_string(), Style::new().fg(token::MUTED)),
        Span::raw(" ".repeat(pad)),
        Span::styled(value.to_string(), Style::new().fg(token::TEXT)),
    ])
}

/// Greedy word wrap. Ratatui's own `Wrap` would do this at render time, but
/// these lines are interleaved with pre-indented ones, and its re-wrap does not
/// preserve the indent — so paragraphs are wrapped here and the widget's wrap
/// only ever catches an overlong single token.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let extra = if line.is_empty() {
                word.chars().count()
            } else {
                word.chars().count() + 1
            };
            if !line.is_empty() && line.chars().count() + extra > width {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        out.push(line);
    }
    out
}

/// The single middle row of `area`, for a centered one-line message.
fn centered_row(area: Rect) -> Rect {
    Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{McpLiveIdentity, McpToolRow};

    fn stripe() -> McpServerDetail {
        McpServerDetail {
            name: "mcp".into(),
            title: Some("Stripe".into()),
            description: Some("Payments, refunds, and balance reads.".into()),
            registry_name: Some("com.stripe/mcp".into()),
            kind: "http".into(),
            endpoint: "https://mcp.stripe.com/v1?api_key=…".into(),
            oauth: Some(true),
            enabled: true,
            connected: true,
            health: Some("live".into()),
            tools: vec![McpToolRow {
                name: "create_refund".into(),
                description: "Refund a charge.\nMore detail.".into(),
                calls: 3,
                ..McpToolRow::default()
            }],
            ..McpServerDetail::default()
        }
    }

    fn rendered(detail: &McpServerDetail) -> String {
        body_lines(detail, 80)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_inspector_answers_what_where_auth_and_tools() {
        let text = rendered(&stripe());
        assert!(text.contains("Payments, refunds"), "description: {text}");
        assert!(text.contains("com.stripe/mcp"), "registry name: {text}");
        assert!(text.contains("mcp.stripe.com"), "endpoint: {text}");
        assert!(text.contains("create_refund"), "tool name: {text}");
        assert!(text.contains("Refund a charge."), "tool summary: {text}");
        assert!(text.contains("3×"), "call count: {text}");
        assert!(text.contains("logged in"), "oauth state: {text}");
    }

    #[test]
    fn a_tool_description_is_summarized_to_its_first_line() {
        let text = rendered(&stripe());
        assert!(
            !text.contains("More detail."),
            "second line of the tool description leaked into the list: {text}"
        );
    }

    #[test]
    fn an_unconnected_server_says_its_tools_are_unknown_not_that_it_has_none() {
        let mut offline = stripe();
        offline.connected = false;
        offline.tools.clear();
        let text = rendered(&offline);
        assert!(text.contains("tool list is unknown"), "{text}");

        // Connected with a genuinely empty surface is a different statement.
        let mut empty = stripe();
        empty.tools.clear();
        assert!(rendered(&empty).contains("advertises no tools"));
    }

    #[test]
    fn the_handshake_identity_is_shown_beside_the_recorded_card() {
        let mut detail = stripe();
        detail.live = Some(McpLiveIdentity {
            name: Some("stripe-mcp".into()),
            version: Some("2.1.0".into()),
            protocol_version: "2025-06-18".into(),
            instructions: Some("Use create_refund to refund.".into()),
            ..McpLiveIdentity::default()
        });
        let text = rendered(&detail);
        assert!(text.contains("stripe-mcp v2.1.0"), "announced: {text}");
        assert!(text.contains("2025-06-18"), "protocol: {text}");
        // The server's own prose is shown, and clearly attributed to it.
        assert!(text.contains("the server's own words"), "{text}");
        assert!(text.contains("Use create_refund"), "{text}");
    }

    #[test]
    fn the_cap_warning_says_the_count_is_a_floor() {
        let mut capped = stripe();
        capped.dropped_tools = 12;
        let text = rendered(&capped);
        assert!(text.contains("at least 12 more"), "{text}");
    }

    /// **The witness (#4441).** The byte budget's warning is exact where the
    /// count cap's is a floor, and names the budget rather than the cap — the
    /// two limits are different and an operator has to know which to go and
    /// look at.
    #[test]
    fn the_budget_warning_is_exact_and_names_the_other_wall() {
        let mut over_budget = stripe();
        over_budget.trimmed_tools = 4;
        let text = rendered(&over_budget);
        assert!(
            text.contains("4 trimmed over the per-server schema byte budget"),
            "{text}"
        );
        assert!(
            !text.contains("at least"),
            "the budget counts exactly: {text}"
        );
    }

    #[test]
    fn every_lookup_state_says_something_different() {
        let mut detail = stripe();
        for (state, needle) in [
            (McpLookupState::Fetching, "looking this server up"),
            (McpLookupState::Found, "saved to .stella/mcp.toml"),
            (McpLookupState::Missing, "no entry under this name"),
            (McpLookupState::Failed("offline".into()), "offline"),
        ] {
            detail.lookup = state;
            let text = rendered(&detail);
            assert!(text.contains(needle), "expected {needle:?} in {text}");
        }
        // Idle says nothing at all — no lookup was attempted.
        detail.lookup = McpLookupState::Idle;
        assert!(!rendered(&detail).contains("registry lookup"));
    }

    #[test]
    fn wrap_breaks_on_words_and_never_loses_one() {
        let wrapped = wrap("alpha beta gamma delta", 11);
        assert_eq!(wrapped, vec!["alpha beta", "gamma delta"]);
        // A single word longer than the width is emitted whole rather than
        // dropped; the widget's own wrap catches it.
        assert_eq!(
            wrap("supercalifragilistic", 5),
            vec!["supercalifragilistic"]
        );
        assert!(wrap("anything", 0).is_empty());
    }

    /// **The witness for the port.** The popup carries the rounded
    /// [`token::BORDER`] frame every other overlay draws, and the status word
    /// in its subtitle is the same colour the list row gave it.
    #[test]
    fn the_popup_takes_the_shared_overlay_frame() {
        let mut ui = DeckUi::default();
        ui.mcp.inspector = Some(crate::views::mcp_tab::McpInspector {
            server: "mcp".into(),
            detail: Some(stripe()),
            scroll: 0,
        });
        let area = Rect::new(0, 0, 100, 24);
        let mut buf = Buffer::empty(area);
        render(&mut ui, area, &mut buf);

        let frame: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(frame.contains('╭') && frame.contains('╮'), "{frame}");
        assert!(frame.contains("esc close"), "{frame}");

        // Cell coordinates, not byte offsets: this frame is full of `╭`, `·`
        // and `─`, so a `str::find` index is nowhere near its column.
        let at = |needle: &str| -> (u16, u16) {
            frame
                .lines()
                .enumerate()
                .find_map(|(y, line)| {
                    let cells: Vec<char> = line.chars().collect();
                    cells
                        .windows(needle.chars().count())
                        .position(|w| w.iter().collect::<String>() == needle)
                        .map(|x| (x as u16, y as u16))
                })
                .unwrap_or_else(|| panic!("no {needle:?} in\n{frame}"))
        };

        assert_eq!(
            buf.cell(at("╭")).map(|c| c.fg),
            Some(token::BORDER),
            "the overlay border is the shared token, not an accent"
        );
        assert_eq!(
            buf.cell(at("live")).map(|c| c.fg),
            Some(token::GREEN),
            "the subtitle's status word is the colour the list row gave it"
        );
    }
}
