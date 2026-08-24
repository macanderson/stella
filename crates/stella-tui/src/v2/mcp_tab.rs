// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The MCP tab — the management surface for external Model Context Protocol
//! servers:
//!
//! ```text
//!  servers · 3 · 2 connected
//! ▸ ● Stripe                 http   21 tools   live  oauth ✓  · 14×
//!       stripe  ·  Payments, refunds, and balance reads.
//!   ○ Linear                 http   0 tools    disabled  o login
//!       linear  ·  Issues, projects, cycles, and documents.
//!
//!  registry · web · s search · new servers land disabled
//!  ↵ tools · ctrl+o inspect · a auth · o login · e enable · x remove
//! ```
//!
//! Each server occupies **two** rows: identity and state on the first, what it
//! is on the second. The second row is the reason this file is shaped the way
//! it is. A configured server's config key is a sanitized alias — installing
//! `com.stripe/mcp` writes `[servers.mcp]` — so a one-row list keyed on that
//! alias renders `mcp [http] not connected` and tells the operator nothing
//! about which vendor they installed or what it can do. The description rides
//! in the config (see `stella_mcp::ServerCard`), the endpoint is the fallback
//! when no description exists, and ctrl+o opens the full detail.
//!
//! Content only: the tab row, pulse row, hint row and status bar are the
//! deck's own bands ([`super::frame`], [`super::pulse`], [`super::status_bar`])
//! and are already drawn around this area. The footer here is not a second
//! hint row — the MCP verbs are unhinted in [`crate::keymap`], so this list is
//! the only place they are written down.
//!
//! State lives entirely in [`McpTabState`], which the driver feeds out of
//! band and [`crate::deck_ui`]'s key handler mutates; the drawing below only
//! reads it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use stella_tui_theme::{glyph, token};

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;
use crate::envelope::{McpSearchOutcome, McpServerDetail, McpServerInfo};

/// The ctrl+o inspector overlay.
pub mod detail;

// ───────────────────────────── the tab's state ─────────────────────────────
//
// Read here, written by the driver's out-of-band snapshots and by
// [`crate::deck_ui::mcp_keys`]. It sits with the paint because the deck's
// own `DeckUi` is a god file closed to growth (#4676 item 5 is what reopens
// the question of where per-tab state belongs).

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
/// It has one limit: `value` is built a keystroke at a time with
/// `String::push`, so each reallocation abandons a buffer this `Drop` can
/// never see. Zeroizing here shortens the plaintext's life; it does not erase
/// every copy of it.
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

/// Draw the tab into the content area the deck carved out.
pub fn render(_model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    let state = &ui.mcp;
    let connected = state.servers.iter().filter(|s| s.connected).count();
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);

    // Truncation is a session-level fact, so it belongs in the header rather
    // than only on the row: an operator scanning tabs should not have to
    // open this one to learn the model is short some tools.
    let truncated = state.servers.iter().filter(|s| s.dropped_tools > 0).count();
    let dropped: usize = state.servers.iter().map(|s| s.dropped_tools).sum();
    let mut head = vec![
        Span::styled(" servers", text),
        Span::styled(
            format!(" · {} · {connected} connected", state.servers.len()),
            muted,
        ),
    ];
    if truncated > 0 {
        head.push(Span::styled(
            format!(" · {truncated} truncated, {dropped} tools dropped"),
            Style::new().fg(token::RED),
        ));
    }

    let mut lines: Vec<Line> = vec![Line::from(head)];
    match state.mode {
        McpMode::Browse => {
            render_browse(state, &mut lines, area.width.saturating_sub(4) as usize);
            render_browse_tail(state, &mut lines);
        }
        McpMode::Search => render_search(state, &mut lines),
        McpMode::Auth => render_auth(state, &mut lines),
    }

    // A transient status line (action feedback), then the keybind footer.
    lines.push(Line::default());
    if let Some(status) = &state.status {
        lines.push(Line::from(Span::styled(
            format!(" {status}"),
            Style::new().fg(token::GOLD),
        )));
    }
    lines.push(footer(state.mode));

    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(area, buf);

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
    let muted = Style::new().fg(token::MUTED);
    if state.servers.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no MCP servers configured · s searches the registry, ↵ installs",
            muted,
        )));
        return;
    }
    for (i, server) in state.servers.iter().enumerate() {
        let selected = i == state.selected;
        lines.push(headline(server, selected));
        lines.push(subline(server, selected, width));
    }
}

/// What follows the server rows: how a login works (only when a row offers
/// one), then the registry line. Apart from [`render_browse`] so the list
/// stays two rows per server for the tests that count them.
fn render_browse_tail(state: &McpTabState, lines: &mut Vec<Line<'static>>) {
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    // How a login works, once, under the list — only when a row offers one.
    if state
        .servers
        .iter()
        .any(|s| s.enabled && !s.connected && s.oauth == Some(false))
    {
        lines.push(Line::from(Span::styled(
            "  login opens the browser · returns via stella:// deep link · token stays in keychain",
            dim,
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled(" registry", Style::new().fg(token::TEXT)),
        Span::styled(" · web · ", muted),
        Span::styled("s", muted),
        Span::styled(" search · new servers land disabled · the handshake shows capabilities before first enable", dim),
    ]));
}

/// One server's name for a surface with no room for a second row — the
/// session-context overlay.
///
/// The publisher's name headlines when one is recorded, with the alias in
/// parentheses. The alias cannot be dropped: it is the tool-namespace segment,
/// so it is what the operator will actually type. It just cannot stand alone,
/// because it is a sanitized last path segment — `com.stripe/mcp` installs as
/// `mcp`, which names nothing.
///
/// The MCP tab's two-row list splits these across lines instead — name on the
/// first, alias and description on the second. This is the one-line form.
pub fn compact_heading(server: &McpServerInfo) -> String {
    match server
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        Some(title) => format!("{title} ({})", server.name),
        None => server.name.clone(),
    }
}

/// A server's first row: selection marker, state glyph, name, transport,
/// tool count, connection state, and the badges that qualify it.
///
/// SPEC 9.3: the dot is gold when connected and dim when not; `oauth ✓` is
/// the one green on the row, `not connected` the one red, and `o login` is
/// gold because pressing it is stella acting.
fn headline(server: &McpServerInfo, selected: bool) -> Line<'static> {
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    let marker = if selected { "▸ " } else { "  " };
    let (dot, dot_style) = if server.enabled && server.connected {
        (glyph::EVENT, Style::new().fg(token::GOLD))
    } else {
        (glyph::QUEUED, dim)
    };
    let name_style = if selected {
        text.bg(token::HL).add_modifier(Modifier::BOLD)
    } else {
        text
    };
    // The publisher's name headlines when there is one; the alias is the
    // routing token, so it never disappears — it moves to the sub-line.
    let heading = server.title.clone().unwrap_or_else(|| server.name.clone());
    let pad = NAME_COLUMN.saturating_sub(heading.chars().count());

    let tools = format!(
        "{} {}",
        server.tool_count,
        if server.tool_count == 1 {
            "tool"
        } else {
            "tools"
        }
    );
    // Connection / health.
    let conn = if !server.enabled {
        Span::styled("disabled", dim)
    } else if server.connected {
        let label = server.health.clone().unwrap_or_else(|| "live".to_string());
        Span::styled(label, muted)
    } else {
        Span::styled("not connected", Style::new().fg(token::RED))
    };

    let mut spans = vec![
        Span::styled(marker.to_string(), Style::new().fg(token::GOLD)),
        Span::styled(dot.to_string(), dot_style),
        Span::raw(" "),
        Span::styled(heading, name_style),
        Span::raw(" ".repeat(pad + 1)),
        Span::styled(format!("{:<5}", server.kind), muted),
        Span::raw("  "),
        Span::styled(format!("{tools:<9}"), text),
        Span::raw("  "),
        conn,
    ];
    if !server.auth_fields.is_empty() {
        spans.push(Span::styled(
            format!("  ⚿ {}", server.auth_fields.join(",")),
            muted,
        ));
    }
    // OAuth state for http servers: logged in (green) or available (`o`).
    match server.oauth {
        Some(true) => spans.push(Span::styled("  oauth ✓", Style::new().fg(token::GREEN))),
        Some(false) => spans.push(Span::styled("  o login", Style::new().fg(token::GOLD))),
        None => {}
    }
    // The dropped count is red: the model has less surface than the
    // operator expects, which is a failure of the row's promise.
    if server.dropped_tools > 0 {
        spans.push(Span::styled(
            format!("  · {} dropped past cap", server.dropped_tools),
            Style::new().fg(token::RED),
        ));
    }
    // The byte budget is the other wall, and the row says which one this
    // server hit. Same red, different word: "past cap" is the tool COUNT,
    // "over budget" the schema BYTES — a server that trips this one is
    // usually nowhere near the count, so a reader given only the first
    // sentence goes looking at the wrong limit (#4441).
    if server.trimmed_tools > 0 {
        spans.push(Span::styled(
            format!("  · {} trimmed over budget", server.trimmed_tools),
            Style::new().fg(token::RED),
        ));
    }
    if server.calls > 0 {
        spans.push(Span::styled(format!("  · {}×", server.calls), muted));
    }
    if server.candidate_safe {
        spans.push(Span::styled("  · candidate-safe", dim));
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
        Style::new().fg(token::MUTED)
    } else {
        Style::new().fg(token::DIM)
    };
    const INDENT: usize = 6;
    let mut spans = vec![Span::raw(" ".repeat(INDENT))];
    let mut used = INDENT;
    // A title took the headline, so the alias — the thing you actually type
    // in `mcp__<alias>__tool` — is shown here rather than lost.
    if server.title.is_some() {
        let prefix = format!("{}  ·  ", server.name);
        used += prefix.chars().count();
        spans.push(Span::styled(prefix, Style::new().fg(token::DIM)));
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
pub(super) fn truncate(text: &str, max: usize) -> String {
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
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);

    lines.push(Line::from(vec![
        Span::styled(" ⌕ ", Style::new().fg(token::GOLD)),
        Span::styled(state.query.clone(), text),
        Span::styled("▌", text),
    ]));
    lines.push(Line::default());

    if state.searching {
        lines.push(Line::from(Span::styled("  searching…", muted)));
        return;
    }
    let Some(outcome) = &state.search else {
        lines.push(Line::from(Span::styled(
            "  type a query and press ↵ to search the registry",
            muted,
        )));
        return;
    };
    if let Some(err) = &outcome.error {
        lines.push(Line::from(Span::styled(
            format!("  search failed: {err}"),
            Style::new().fg(token::RED),
        )));
        return;
    }
    if outcome.items.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  no servers matching “{}”", outcome.query),
            muted,
        )));
        return;
    }
    for (i, item) in outcome.items.iter().enumerate() {
        let selected = i == state.search_selected;
        let name_style = if selected {
            text.bg(token::HL).add_modifier(Modifier::BOLD)
        } else {
            text
        };
        let mut spans = vec![
            Span::styled(
                if selected { "▸ " } else { "  " }.to_string(),
                Style::new().fg(token::GOLD),
            ),
            Span::styled(item.name.clone(), name_style),
            Span::styled(format!("  [{}]", item.kinds), muted),
        ];
        if item.installed {
            spans.push(Span::styled("  installed", Style::new().fg(token::GREEN)));
        }
        lines.push(Line::from(spans));
        if !item.description.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("      {}", item.description),
                dim,
            )));
        }
    }
    if outcome.has_more {
        lines.push(Line::from(Span::styled(
            "  ⋯ more results (refine the query)",
            dim,
        )));
    }
}

fn render_auth(state: &McpTabState, lines: &mut Vec<Line<'static>>) {
    lines.push(Line::from(vec![
        Span::styled("  auth ", Style::new().fg(token::GOLD)),
        Span::styled(state.auth.server.clone(), Style::new().fg(token::TEXT)),
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
        "  the value is stored in .stella/mcp.toml and never logged",
        Style::new().fg(token::DIM),
    )));
}

/// One field of the auth prompt. When `mask`, the value renders as bullets.
fn prompt_line(label: &str, value: &str, active: bool, mask: bool) -> Line<'static> {
    let shown = if mask {
        "•".repeat(value.chars().count())
    } else {
        value.to_string()
    };
    let mut spans = vec![
        Span::styled(label.to_string(), Style::new().fg(token::MUTED)),
        Span::styled(shown, Style::new().fg(token::TEXT)),
    ];
    if active {
        spans.push(Span::styled("▏", Style::new().fg(token::GOLD)));
    }
    Line::from(spans)
}

fn footer(mode: McpMode) -> Line<'static> {
    let pairs: &[(&str, &str)] = match mode {
        McpMode::Browse => &[
            ("↵", "tools"),
            ("ctrl+o", "inspect"),
            ("a", "auth"),
            ("o", "login"),
            ("e", "enable"),
            ("x", "remove"),
            ("r", "refresh"),
            ("s", "search"),
        ],
        McpMode::Search => &[
            ("type", "query"),
            ("↑↓", "results"),
            ("↵", "search / install"),
            ("esc", "back"),
        ],
        McpMode::Auth => &[("↵", "next / save"), ("esc", "cancel")],
    };
    let key = Style::new().fg(token::MUTED);
    let dim = Style::new().fg(token::DIM);
    let mut spans = vec![Span::raw(" ")];
    for (i, (k, desc)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(Span::styled((*k).to_string(), key));
        spans.push(Span::styled(format!(" {desc}"), dim));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::McpSearchItem;

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

    /// **The witness (#4441).** The byte budget is on the row, in its own
    /// words. Both walls at once on one server, because the failure this
    /// replaces was a row that could only ever name the count cap: a server
    /// that blew the byte budget and nothing else read as perfectly healthy
    /// the moment the connect notice scrolled away.
    #[test]
    fn a_row_names_which_wall_the_server_hit() {
        let over_budget = McpServerInfo {
            trimmed_tools: 4,
            ..stripe()
        };
        let text = rows(&[over_budget]).join("\n");
        assert!(text.contains("4 trimmed over budget"), "{text}");
        assert!(
            !text.contains("past cap"),
            "the byte budget is not the count cap: {text}"
        );

        let both = McpServerInfo {
            dropped_tools: 12,
            trimmed_tools: 4,
            ..stripe()
        };
        let text = rows(&[both]).join("\n");
        assert!(text.contains("12 dropped past cap"), "{text}");
        assert!(text.contains("4 trimmed over budget"), "{text}");
    }

    #[test]
    fn the_footer_advertises_the_inspector() {
        assert!(flat(&footer(McpMode::Browse)).contains("ctrl+o"));
    }

    /// **The witness for the port.** Every span the tab paints resolves to a
    /// [`stella_tui_theme::token`] colour. The search and auth panes were the
    /// last two surfaces here still painting from the pre-SPEC-5 ramp, and a
    /// row that keeps its words while changing its metal is exactly the
    /// regression a `contains` assertion cannot see.
    #[test]
    fn every_pane_paints_from_the_token_palette() {
        const PALETTE: [ratatui::style::Color; 9] = [
            token::TEXT,
            token::MUTED,
            token::DIM,
            token::GOLD,
            token::GREEN,
            token::RED,
            token::HL,
            token::SILVER,
            token::BORDER,
        ];
        let mut state = McpTabState {
            servers: vec![stripe()],
            query: "stripe".into(),
            search: Some(McpSearchOutcome {
                query: "stripe".into(),
                items: vec![McpSearchItem {
                    name: "com.stripe/mcp".into(),
                    description: "Payments.".into(),
                    kinds: "http".into(),
                    installed: true,
                }],
                has_more: true,
                error: None,
            }),
            ..McpTabState::default()
        };
        for mode in [McpMode::Browse, McpMode::Search, McpMode::Auth] {
            state.mode = mode;
            let mut lines = Vec::new();
            match mode {
                McpMode::Browse => render_browse(&state, &mut lines, 80),
                McpMode::Search => render_search(&state, &mut lines),
                McpMode::Auth => render_auth(&state, &mut lines),
            }
            for line in &lines {
                for span in &line.spans {
                    for colour in [span.style.fg, span.style.bg].into_iter().flatten() {
                        assert!(
                            PALETTE.contains(&colour),
                            "{mode:?}: {:?} is painted {colour:?}, which is not a token",
                            span.content
                        );
                    }
                }
            }
        }
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
