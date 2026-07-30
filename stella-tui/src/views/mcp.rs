//! MCP tab — the management surface for external Model Context Protocol
//! servers: a live dashboard of configured servers (enabled/connected/health,
//! configured auth, per-tool call counts) plus in-tab registry search,
//! install, per-session enable/disable, auth, remove, and a ctrl+o inspector.
//!
//! Each server occupies **two** rows: identity and state on the first, what it
//! is on the second. The second row is the reason this file is shaped the way
//! it is. A configured server's config key is a sanitized alias — installing
//! `com.stripe/mcp` writes `[servers.mcp]` — so a one-row list keyed on that
//! alias renders `mcp [http] not connected` and tells the operator nothing
//! about which vendor they installed or what it can do. The description now
//! rides in the config (see `stella_mcp::ServerCard`), the endpoint is the
//! fallback when no description exists, and ctrl+o opens the full detail.
//!
//! State lives entirely in [`McpTabState`] (a field on `DeckUi`); the driver
//! feeds it out-of-band snapshots ([`crate::Inbound::McpServers`] /
//! [`crate::Inbound::McpSearchResults`] / [`crate::Inbound::McpDetail`]) and
//! services the actions the key handler emits. The auth-value buffer is
//! redacted in `Debug` so it never reaches the deck's debug log.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;
use crate::envelope::{McpSearchOutcome, McpServerDetail, McpServerInfo};
use crate::theme;

/// The ctrl+o inspector overlay.
mod detail;

/// Which sub-mode the MCP tab is in — browsing the configured list, typing a
/// registry search, or entering an auth credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum McpMode {
    #[default]
    Browse,
    Search,
    Auth,
}

/// The two steps of the in-tab auth prompt: name the credential, then enter its
/// (masked) value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthStep {
    #[default]
    Field,
    Value,
}

/// The in-progress auth prompt. `value`'s `Debug` is redacted so it never
/// appears in a log even though `DeckUi` derives `Debug`, and it is wiped on
/// drop so an abandoned prompt (Esc, or a completed one being replaced by
/// `AuthPrompt::default()`) does not leave the typed credential legible in
/// freed heap.
///
/// One honest limit: `value` is built a keystroke at a time with `String::push`,
/// so each reallocation abandons a buffer this `Drop` can never see. Zeroizing
/// here shortens the plaintext's life; it does not claim to erase every copy.
#[derive(Clone, Default)]
pub struct AuthPrompt {
    pub server: String,
    pub field: String,
    pub value: String,
    pub step: AuthStep,
}

impl Drop for AuthPrompt {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        self.value.zeroize();
    }
}

impl std::fmt::Debug for AuthPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthPrompt")
            .field("server", &self.server)
            .field("field", &self.field)
            .field("value", &"<redacted>")
            .field("step", &self.step)
            .finish()
    }
}

/// The ctrl+o inspector overlay: one server's full detail, scrolled.
///
/// Held apart from [`McpTabState::servers`] rather than folded into the row it
/// describes because it is assembled on demand — it carries the live
/// handshake, the advertised tool table, and (optionally) a registry lookup,
/// none of which belong in a snapshot pushed on every toggle.
#[derive(Debug, Clone)]
pub struct McpInspector {
    /// The alias whose detail is being shown. Kept even before `detail`
    /// arrives so a late reply for a *different* server cannot overwrite the
    /// one on screen.
    pub server: String,
    /// The assembled detail; `None` renders a loading state.
    pub detail: Option<McpServerDetail>,
    /// Vertical scroll offset in lines, clamped to content at render time.
    pub scroll: u16,
}

/// All MCP-tab view state.
#[derive(Debug, Clone, Default)]
pub struct McpTabState {
    /// The configured servers snapshot (out-of-band, from the driver).
    pub servers: Vec<McpServerInfo>,
    /// Highlighted row in the configured list.
    pub selected: usize,
    pub mode: McpMode,
    /// The registry-search query buffer (Search mode).
    pub query: String,
    /// The most recent search outcome (results or an error).
    pub search: Option<McpSearchOutcome>,
    /// Highlighted row among the search results.
    pub search_selected: usize,
    /// A search request is in flight (show a spinner-ish label).
    pub searching: bool,
    /// The in-progress auth prompt (Auth mode).
    pub auth: AuthPrompt,
    /// A transient one-line status/feedback message (cleared on next snapshot).
    pub status: Option<String>,
    /// The open ctrl+o inspector, if any. Modal over every mode: it is the
    /// topmost surface and Esc closes it.
    pub inspector: Option<McpInspector>,
}

impl McpTabState {
    /// The currently-highlighted configured server, if any.
    pub fn selected_server(&self) -> Option<&McpServerInfo> {
        self.servers.get(self.selected)
    }

    /// Apply an inbound detail, ignoring one that names a server other than
    /// the open inspector's.
    ///
    /// The guard matters because a detail can arrive *after* a registry
    /// round-trip: by then the operator may have closed the inspector and
    /// opened another server's, and painting the late reply over it would
    /// silently mislabel every field on screen.
    pub fn apply_detail(&mut self, detail: McpServerDetail) {
        if let Some(inspector) = self.inspector.as_mut()
            && inspector.server == detail.name
        {
            inspector.detail = Some(detail);
        }
    }

    /// The currently-highlighted search result name, if any.
    pub fn selected_search_name(&self) -> Option<&str> {
        self.search
            .as_ref()
            .and_then(|o| o.items.get(self.search_selected))
            .map(|i| i.name.as_str())
    }

    /// Whether the current search results match the current query (so a second
    /// Enter should install rather than re-search).
    pub fn results_match_query(&self) -> bool {
        self.search.as_ref().is_some_and(|o| {
            o.error.is_none() && o.query == self.query.trim() && !o.items.is_empty()
        })
    }
}

pub fn render(_model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    let state = &ui.mcp;
    let connected = state.servers.iter().filter(|s| s.connected).count();
    // Truncation is a session-level fact, so it belongs in the title rather
    // than only on the row: an operator scanning tabs should not have to open
    // this one to learn the model is short some tools.
    let truncated = state.servers.iter().filter(|s| s.dropped_tools > 0).count();
    let dropped: usize = state.servers.iter().map(|s| s.dropped_tools).sum();
    let title = if truncated == 0 {
        format!(
            " MCP — {} configured / {} connected ",
            state.servers.len(),
            connected
        )
    } else {
        format!(
            " MCP — {} configured / {} connected / {truncated} truncated, {dropped} tools dropped ",
            state.servers.len(),
            connected
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(theme::rule());
    let inner = block.inner(area);
    block.render(area, buf);

    let mut lines: Vec<Line> = Vec::new();
    match state.mode {
        McpMode::Browse => render_browse(state, &mut lines, inner.width as usize),
        McpMode::Search => render_search(state, &mut lines),
        McpMode::Auth => render_auth(state, &mut lines),
    }

    // A transient status line (action feedback), then the keybind footer.
    lines.push(Line::default());
    if let Some(status) = &state.status {
        lines.push(Line::from(Span::styled(
            format!("  {status}"),
            Style::default().fg(theme::ACCENT),
        )));
    }
    lines.push(footer(state.mode));

    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(inner, buf);

    // The inspector is the topmost surface — drawn over the list it describes,
    // after it, so it is never clipped by the list's own layout.
    if ui.mcp.inspector.is_some() {
        detail::render(ui, area, buf);
    }
}

/// How wide the name column is padded to, so the badges that follow line up
/// into readable columns instead of ragging with each server's name length.
/// A name longer than this pushes its own row out rather than widening every
/// other one — the common case is short aliases and one long outlier.
const NAME_COLUMN: usize = 22;

fn render_browse(state: &McpTabState, lines: &mut Vec<Line<'static>>, width: usize) {
    if state.servers.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No MCP servers configured.",
            theme::muted(),
        )));
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "  Press s to search a registry, then Enter to install one.",
            theme::muted(),
        )));
        return;
    }
    for (i, server) in state.servers.iter().enumerate() {
        let selected = i == state.selected;
        lines.push(headline(server, selected));
        lines.push(subline(server, selected, width));
    }
}

/// A server's first row: selection marker, enable glyph, name, transport,
/// connection state, and the badges that qualify it.
fn headline(server: &McpServerInfo, selected: bool) -> Line<'static> {
    let marker = if selected { "▸ " } else { "  " };
    // Enabled/disabled glyph.
    let (enabled_glyph, enabled_style) = if server.enabled {
        ("●", Style::default().fg(theme::SUCCESS_BRIGHT))
    } else {
        ("○", theme::muted())
    };
    let name_style = if selected {
        Style::default()
            .fg(theme::INK)
            .bg(theme::SELECT_BG)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::INK).add_modifier(Modifier::BOLD)
    };
    // The publisher's name headlines when there is one; the alias is the
    // routing token, so it never disappears — it moves to the sub-line.
    let heading = server.title.clone().unwrap_or_else(|| server.name.clone());
    let pad = NAME_COLUMN.saturating_sub(heading.chars().count());

    // Connection / health.
    let conn = if !server.enabled {
        Span::styled("disabled", theme::muted())
    } else if server.connected {
        let label = server.health.clone().unwrap_or_else(|| "live".to_string());
        Span::styled(label, Style::default().fg(theme::SUCCESS))
    } else {
        Span::styled("not connected", Style::default().fg(theme::WARNING_BRIGHT))
    };

    let mut spans = vec![
        Span::raw(marker),
        Span::styled(enabled_glyph, enabled_style),
        Span::raw(" "),
        Span::styled(heading, name_style),
        Span::raw(" ".repeat(pad + 1)),
        Span::styled(format!("{:<5}", server.kind), theme::muted()),
        Span::raw("  "),
        conn,
    ];
    if !server.auth_fields.is_empty() {
        spans.push(Span::styled(
            format!("  ⚿ {}", server.auth_fields.join(",")),
            Style::default().fg(theme::VIOLET),
        ));
    }
    // OAuth state for http servers: logged in (green) or available (`o`).
    match server.oauth {
        Some(true) => spans.push(Span::styled(
            "  ⚿ oauth ✓",
            Style::default().fg(theme::SUCCESS),
        )),
        Some(false) => spans.push(Span::styled("  ⚿ oauth: o to log in", theme::muted())),
        None => {}
    }
    spans.push(Span::styled(
        format!("  · {} tools", server.tool_count),
        theme::muted(),
    ));
    // WARNING, not DANGER: the server is healthy and its kept tools route.
    // Same colour as `not connected` because the consequence is the same
    // shape — the model has less surface than the operator expects.
    if server.dropped_tools > 0 {
        spans.push(Span::styled(
            format!("  · {} dropped past cap", server.dropped_tools),
            Style::default().fg(theme::WARNING_BRIGHT),
        ));
    }
    if server.calls > 0 {
        spans.push(Span::styled(
            format!("  · {}×", server.calls),
            Style::default().fg(theme::ACCENT),
        ));
    }
    if server.candidate_safe {
        spans.push(Span::styled("  · candidate-safe", theme::muted()));
    }
    Line::from(spans)
}

/// A server's second row: what it is, in words.
///
/// This is the line the tab was missing. An alias like `mcp` with `[http]`
/// beside it is unidentifiable — learning which vendor was behind one meant
/// starting its OAuth flow and reading the *browser* to find out. The
/// description says it outright when one is recorded; failing that the
/// endpoint does, because a URL names its host and a spawn command names its
/// package. Indented under the name so the two rows read as one entry.
///
/// Truncated rather than wrapped: a two-line entry that sometimes takes three
/// lines makes the list jump as servers connect, and the full text is one
/// ctrl+o away.
fn subline(server: &McpServerInfo, selected: bool, width: usize) -> Line<'static> {
    let style = if selected {
        Style::default().fg(theme::ACCENT)
    } else {
        theme::muted()
    };
    const INDENT: usize = 6;
    let mut spans = vec![Span::raw(" ".repeat(INDENT))];
    let mut used = INDENT;
    // A title took the headline, so the alias — the thing you actually type
    // in `mcp__<alias>__tool` — is shown here rather than lost.
    if server.title.is_some() {
        let prefix = format!("{}  ·  ", server.name);
        used += prefix.chars().count();
        spans.push(Span::styled(prefix, theme::muted()));
    }
    let body = match server.description.as_deref() {
        Some(desc) if !desc.trim().is_empty() => desc.trim(),
        _ => server.endpoint.as_str(),
    };
    spans.push(Span::styled(
        truncate(body, width.saturating_sub(used)),
        style,
    ));
    Line::from(spans)
}

/// Char-safe truncation with an ellipsis. A `max` under 2 yields an empty
/// string rather than a lone `…`, which would say less than nothing.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    if max < 2 {
        return String::new();
    }
    let head: String = text.chars().take(max - 1).collect();
    format!("{head}…")
}

fn render_search(state: &McpTabState, lines: &mut Vec<Line<'static>>) {
    let query_line = Line::from(vec![
        Span::styled("  search ", theme::accent()),
        Span::styled(state.query.clone(), Style::default().fg(theme::INK)),
        Span::styled("▏", Style::default().fg(theme::ACCENT)),
    ]);
    lines.push(query_line);
    lines.push(Line::default());

    if state.searching {
        lines.push(Line::from(Span::styled("  searching…", theme::accent())));
        return;
    }
    let Some(outcome) = &state.search else {
        lines.push(Line::from(Span::styled(
            "  Type a query and press Enter to search the registry.",
            theme::muted(),
        )));
        return;
    };
    if let Some(err) = &outcome.error {
        lines.push(Line::from(Span::styled(
            format!("  search failed: {err}"),
            Style::default().fg(theme::DANGER_BRIGHT),
        )));
        return;
    }
    if outcome.items.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  no servers matching “{}”", outcome.query),
            theme::muted(),
        )));
        return;
    }
    for (i, item) in outcome.items.iter().enumerate() {
        let selected = i == state.search_selected;
        let marker = if selected { "▸ " } else { "  " };
        let name_style = if selected {
            Style::default()
                .fg(theme::INK)
                .bg(theme::SELECT_BG)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::INK)
        };
        let mut spans = vec![
            Span::raw(marker),
            Span::styled(item.name.clone(), name_style),
            Span::styled(format!("  [{}]", item.kinds), theme::muted()),
        ];
        if item.installed {
            spans.push(Span::styled(
                "  installed",
                Style::default().fg(theme::SUCCESS),
            ));
        }
        lines.push(Line::from(spans));
        if !item.description.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("      {}", item.description),
                theme::muted(),
            )));
        }
    }
    if outcome.has_more {
        lines.push(Line::from(Span::styled(
            "  … more results (refine the query)",
            theme::muted(),
        )));
    }
}

fn render_auth(state: &McpTabState, lines: &mut Vec<Line<'static>>) {
    lines.push(Line::from(vec![
        Span::styled("  auth ", theme::accent()),
        Span::styled(state.auth.server.clone(), Style::default().fg(theme::INK)),
    ]));
    lines.push(Line::default());
    let field_active = state.auth.step == AuthStep::Field;
    lines.push(prompt_line(
        "  credential (env var / header): ",
        &state.auth.field,
        field_active,
        false,
    ));
    lines.push(prompt_line(
        "  value: ",
        &state.auth.value,
        !field_active,
        true,
    ));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "  The value is stored in .stella/mcp.toml and never logged.",
        theme::muted(),
    )));
}

/// One field of the auth prompt. When `mask`, the value renders as bullets.
fn prompt_line(label: &str, value: &str, active: bool, mask: bool) -> Line<'static> {
    let shown = if mask {
        "•".repeat(value.chars().count())
    } else {
        value.to_string()
    };
    let value_style = Style::default().fg(theme::INK);
    let mut spans = vec![
        Span::styled(label.to_string(), theme::accent()),
        Span::styled(shown, value_style),
    ];
    if active {
        spans.push(Span::styled("▏", Style::default().fg(theme::ACCENT)));
    }
    Line::from(spans)
}

fn footer(mode: McpMode) -> Line<'static> {
    let pairs: &[(&str, &str)] = match mode {
        McpMode::Browse => &[
            ("↑↓", "select"),
            ("ctrl+o", "inspect"),
            ("e/␣", "enable/disable"),
            ("s", "search registry"),
            ("a", "auth"),
            ("o", "oauth login"),
            ("x", "remove"),
            ("r", "refresh"),
        ],
        McpMode::Search => &[
            ("type", "query"),
            ("↑↓", "results"),
            ("enter", "search/install"),
            ("esc", "back"),
        ],
        McpMode::Auth => &[("enter", "next/save"), ("esc", "cancel")],
    };
    let mut spans = vec![Span::raw("  ")];
    for (key, desc) in pairs {
        spans.push(Span::styled(
            format!(" {key} "),
            Style::default()
                .fg(theme::VIOLET)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!("{desc}  "), theme::muted()));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn rows(servers: &[McpServerInfo]) -> Vec<String> {
        let state = McpTabState {
            servers: servers.to_vec(),
            ..McpTabState::default()
        };
        let mut lines = Vec::new();
        render_browse(&state, &mut lines, 80);
        lines.iter().map(flat).collect()
    }

    fn stripe() -> McpServerInfo {
        McpServerInfo {
            name: "mcp".into(),
            kind: "http".into(),
            endpoint: "https://mcp.stripe.com/v1".into(),
            enabled: true,
            oauth: Some(false),
            ..McpServerInfo::default()
        }
    }

    /// The reported bug: an aliased server rendered as `mcp [http] not
    /// connected`, and the only way to learn it was Stripe was to start its
    /// OAuth flow and read the browser.
    #[test]
    fn a_row_says_what_the_server_is_even_with_no_description() {
        let text = rows(&[stripe()]).join("\n");
        assert!(
            text.contains("mcp.stripe.com"),
            "the endpoint is the identity of last resort: {text}"
        );
    }

    #[test]
    fn a_recorded_description_outranks_the_endpoint_and_the_alias_survives() {
        let described = McpServerInfo {
            title: Some("Stripe".into()),
            description: Some("Payments, refunds, and balance reads.".into()),
            ..stripe()
        };
        let lines = rows(&[described]);
        assert!(lines[0].contains("Stripe"), "headline title: {lines:?}");
        assert!(
            lines[1].contains("Payments, refunds"),
            "description wins the sub-line: {lines:?}"
        );
        assert!(
            lines[1].contains("mcp"),
            "the alias is the routing token and must stay visible: {lines:?}"
        );
        assert!(
            !lines.join("\n").contains("mcp.stripe.com"),
            "endpoint is the fallback, not an addition: {lines:?}"
        );
    }

    #[test]
    fn every_server_occupies_exactly_two_rows() {
        let lines = rows(&[stripe(), stripe()]);
        assert_eq!(lines.len(), 4, "two rows each, no more: {lines:?}");
    }

    #[test]
    fn a_long_description_is_truncated_rather_than_wrapped() {
        let wordy = McpServerInfo {
            description: Some("x".repeat(400)),
            ..stripe()
        };
        let lines = rows(&[wordy]);
        assert_eq!(lines.len(), 2, "wrapping would make the list jump");
        assert!(lines[1].chars().count() <= 80, "over width: {lines:?}");
        assert!(lines[1].ends_with('…'));
    }

    #[test]
    fn state_badges_survive_the_two_row_layout() {
        let mut disabled = stripe();
        disabled.enabled = false;
        disabled.dropped_tools = 12;
        disabled.auth_fields = vec!["Authorization".into()];
        let text = rows(&[disabled]).join("\n");
        assert!(text.contains("disabled"), "{text}");
        assert!(text.contains("12 dropped past cap"), "{text}");
        assert!(text.contains("Authorization"), "{text}");
    }

    #[test]
    fn the_footer_advertises_the_inspector() {
        assert!(flat(&footer(McpMode::Browse)).contains("ctrl+o"));
    }

    #[test]
    fn a_late_detail_for_another_server_never_overwrites_the_open_one() {
        let mut state = McpTabState {
            inspector: Some(McpInspector {
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
        assert!(
            state.inspector.as_ref().unwrap().detail.is_none(),
            "a reply for `mcp` painted over the inspector showing `fs`"
        );
        state.apply_detail(McpServerDetail {
            name: "fs".into(),
            ..McpServerDetail::default()
        });
        assert!(state.inspector.unwrap().detail.is_some());
    }
}
