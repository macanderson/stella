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
//!
//!  installed stripe
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
//! Three bands, the shape the other ported panes carry: the mode's own pane,
//! then the driver's last word, then the keys. The bottom two are one line each
//! and pinned to the floor, so the verbs sit on the same row whether two
//! servers are configured or twenty — a legend that slides up the pane with the
//! content is one the eye has to hunt for every time.
//!
//! The deck's own bands ([`super::frame`], [`super::pulse`],
//! [`super::status_bar`]) are drawn around this area. The key row here is not a
//! second copy of the deck's hint row — the MCP verbs are unhinted in
//! [`crate::keymap`], so this pane is the only place they are written down.
//!
//! State lives entirely in [`McpTabState`], which the driver feeds out of
//! band and [`crate::deck_ui`]'s key handler mutates; the drawing below only
//! reads it.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use stella_tui_theme::{glyph, token};

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;
use crate::envelope::{McpSearchOutcome, McpServerDetail, McpServerInfo};

/// The ctrl+o inspector overlay.
pub mod detail;
/// The first-enable capability handshake (SPEC §9.3).
pub mod handshake;

pub use handshake::HandshakeGate;

// ───────────────────────────── the tab's state ─────────────────────────────
//
// Read here, written by the driver's out-of-band snapshots and by
// [`crate::deck_ui::mcp_keys`]. It sits with the paint because the deck's
// own `DeckUi` is a god file closed to growth (#4676 item 5 is what reopens
// the question of where per-tab state belongs).

/// Which sub-mode the MCP tab is in — browsing the configured list, typing a
/// registry search, entering an auth credential, or reading a server's
/// declared capabilities before its first enable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum McpMode {
    #[default]
    Browse,
    Search,
    Auth,
    /// SPEC §9.3's first-enable handshake: what this server declares, and the
    /// grant it cannot be used without.
    Handshake,
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
    /// The open first-enable handshake gate, if any — set together with
    /// [`McpMode::Handshake`].
    pub handshake: Option<HandshakeGate>,
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
        if let Some(gate) = self.handshake.as_mut()
            && gate.server == detail.name
        {
            gate.detail = Some(detail.clone());
        }
        if let Some(inspector) = self.inspector.as_mut()
            && inspector.server == detail.name
        {
            inspector.detail = Some(detail);
        }
    }

    /// Whether the highlighted server still owes a capability grant, so `e`
    /// opens the handshake instead of toggling it.
    pub fn selection_needs_grant(&self) -> bool {
        self.selected_server().is_some_and(|s| !s.granted)
    }

    /// The currently-highlighted search result name, if any.
    pub fn selected_search_name(&self) -> Option<&str> {
        self.search
            .as_ref()
            .and_then(|o| o.items.get(self.search_selected))
            .map(|i| i.name.as_str())
    }

    /// Why the highlighted search result cannot be installed, or `None` when
    /// it can.
    ///
    /// Phrased for the operator who just pressed the key, and named after the
    /// refusal rather than the state, because the caller's question is "may I
    /// install this" and every answer to it belongs in one place.
    pub fn selected_search_refusal(&self) -> Option<String> {
        let item = self
            .search
            .as_ref()
            .and_then(|o| o.items.get(self.search_selected))?;
        (!item.installable())
            .then(|| format!("{}: {} — not installed", item.name, item.signature.label()))
    }

    /// Whether the current search results match the current query (so a second
    /// Enter should install rather than re-search).
    pub fn results_match_query(&self) -> bool {
        self.search.as_ref().is_some_and(|o| {
            o.error.is_none() && o.query == self.query.trim() && !o.items.is_empty()
        })
    }
}

/// Draw the tab into the content area the deck carved out: the mode's pane,
/// the driver's last word, and the pinned key row.
pub fn render(_model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    if area.width < 4 || area.height == 0 {
        return; // no readable pane fits — draw nothing rather than garbage
    }
    let bands = Layout::vertical([
        Constraint::Min(0),    // header · the mode's pane
        Constraint::Length(1), // the driver's last word
        Constraint::Length(1), // keys
    ])
    .split(area);

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
            render_browse(state, &mut lines, bands[0].width.saturating_sub(4) as usize);
            render_browse_tail(state, &mut lines);
        }
        McpMode::Search => render_search(state, &mut lines),
        McpMode::Auth => render_auth(state, &mut lines),
        McpMode::Handshake => handshake::render(state, &mut lines, bands[0].height as usize),
    }
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(bands[0], buf);

    render_status(state, bands[1], buf);
    render_keys(state, bands[2], buf);

    // The inspector is the topmost surface — drawn over the whole area, after
    // the bands, so it is never clipped by the list's own layout.
    if ui.mcp.inspector.is_some() {
        detail::render(ui, area, buf);
    }
}

/// The driver's last word on an install, a login, or an enable — or the local
/// refusal that replaced it. Blank while there is nothing to say.
fn render_status(state: &McpTabState, area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let Some(status) = &state.status else {
        return;
    };
    Paragraph::new(Line::from(Span::styled(
        format!(" {status}"),
        Style::new().fg(token::GOLD),
    )))
    .render(area, buf);
}

/// The pinned key row: the verbs of whichever mode holds the keyboard.
///
/// Blank while the inspector is up. The popup is centered and shorter than the
/// pane, so this row stays uncovered — and the inspector is modal
/// (`crate::deck_ui::mcp_keys` returns before every mode), so `a auth` and
/// `x remove` would be advertised at the moment they do nothing. Its own verbs
/// are in its border title, which is where an overlay's legend belongs.
fn render_keys(state: &McpTabState, area: Rect, buf: &mut Buffer) {
    if area.height == 0 || state.inspector.is_some() {
        return;
    }
    Paragraph::new(footer(state.mode)).render(area, buf);
}

/// How wide the name column is padded to, so the badges that follow line up
/// into readable columns instead of ragging with each server's name length.
/// A name longer than this pushes its own row out rather than widening every
/// other one — the common case is short aliases and one long outlier.
const NAME_COLUMN: usize = 22;

/// SPEC §9.3's caption under the pinned graph row. It sits with the row it
/// explains rather than at the foot of the pane: a sentence about why one
/// entry outranks the others says nothing four rows away from it.
pub(super) const PIN_CAPTION: &str = "graph is pinned · it is the product, not an integration";

/// The order the list is painted in: the graph server first, then everything
/// else in the order the driver delivered (`mcp.toml`'s stable alphabetical
/// keys).
///
/// Indices rather than rows, because [`McpTabState::selected`] indexes the
/// unsorted snapshot — every key verb acts on `state.servers[selected]`, so
/// re-ordering the paint must not re-order what the keys address. The
/// alternative, sorting the snapshot itself, would make `x` remove whichever
/// server happened to be painted where the cursor was.
fn paint_order(servers: &[McpServerInfo]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..servers.len()).collect();
    // Stable, so the delivered order survives underneath the pin.
    order.sort_by_key(|&i| !servers[i].is_graph());
    order
}

fn render_browse(state: &McpTabState, lines: &mut Vec<Line<'static>>, width: usize) {
    let muted = Style::new().fg(token::MUTED);
    if state.servers.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no MCP servers configured · s searches the registry, ↵ installs",
            muted,
        )));
        return;
    }
    for i in paint_order(&state.servers) {
        let server = &state.servers[i];
        let selected = i == state.selected;
        lines.push(headline(server, selected));
        lines.push(subline(server, selected, width));
        if server.is_graph() {
            lines.push(Line::from(Span::styled(
                format!("      {PIN_CAPTION}"),
                Style::new().fg(token::DIM),
            )));
        }
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
        // SPEC §9.3's latency column, between the tool count and the state.
        // A fixed-width slot whether or not there is a number, so the state
        // word starts on the same column for every row — the whole point of
        // padding the name column above.
        Span::styled(format!("{:>6}", latency(server)), muted),
        Span::raw("  "),
        conn,
    ];
    // The graph is stella's own, and the pin is only legible if the row says
    // why it is at the top.
    if server.is_graph() {
        spans.push(Span::styled("  · pinned", Style::new().fg(token::GOLD)));
    }
    // A server whose handshake has not been granted is connected and useless:
    // the model is never told its tools exist. Red and in words, because this
    // is the row's most consequential state and the operator has to be able to
    // tell it from a healthy one at a glance (SPEC §13 — never colour alone).
    if !server.granted {
        spans.push(Span::styled("  · ungranted", Style::new().fg(token::RED)));
    }
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

/// The latency cell: whole milliseconds of the connect handshake's round trip,
/// or blank.
///
/// Blank rather than a dash or a zero when the number is unknown, and blank
/// for a server that is not connected even if one was once measured — the
/// column answers "how far away is this server right now", and a stale figure
/// beside `not connected` would answer a question nobody asked.
fn latency(server: &McpServerInfo) -> String {
    match server.latency_ms {
        Some(ms) if server.connected && server.enabled => format!("{ms}ms"),
        _ => String::new(),
    }
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
            // Who the registry says published this. Ahead of the counts,
            // because it is the one that decides whether the rest matters.
            Span::styled(format!("  {}", item.tier.label()), muted),
            Span::styled(format!("  {}", installs(item.installs)), dim),
        ];
        // Signed is the only state that lets `↵` do anything, so it is the
        // only green here; the two blocked states are red and say `blocked`
        // in words.
        spans.push(if item.installable() {
            Span::styled(
                format!("  {}", item.signature.label()),
                Style::new().fg(token::GREEN),
            )
        } else {
            Span::styled(
                format!("  {}", item.signature.label()),
                Style::new().fg(token::RED),
            )
        });
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
    // Why a red row cannot be installed, once, under the list — and only when
    // the page actually holds one.
    if outcome.items.iter().any(|i| !i.installable()) {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "  a blocked entry is one the registry does not vouch for · installing it would \
             spawn a stranger's command",
            dim,
        )));
    }
}

/// The installs cell. An absent count says so; it never becomes a `0`, which
/// would claim nobody runs a server the registry simply does not count.
///
/// Thousands are abbreviated (`9.1k`) so the column stays one width for a
/// registry that counts in millions.
fn installs(count: Option<u64>) -> String {
    let Some(count) = count else {
        return "installs unknown".to_string();
    };
    match count {
        n if n < 1_000 => format!("{n} installs"),
        n if n < 1_000_000 => format!("{:.1}k installs", n as f64 / 1_000.0),
        n => format!("{:.1}M installs", n as f64 / 1_000_000.0),
    }
}

/// The two-step credential prompt: which field, then its value.
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
        McpMode::Handshake => &[("↑↓", "read"), ("g", "grant & enable"), ("esc", "deny")],
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
    use crate::envelope::{GRAPH_SERVER, McpSearchItem, McpSignature, McpSourceTier};

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
            granted: true,
            oauth: Some(false),
            ..McpServerInfo::default()
        }
    }

    /// The pinned row: stella's own graph server, connected and measured.
    fn graph() -> McpServerInfo {
        McpServerInfo {
            name: GRAPH_SERVER.into(),
            kind: "stdio".into(),
            endpoint: "stella-graph-mcp".into(),
            enabled: true,
            connected: true,
            granted: true,
            latency_ms: Some(8),
            tool_count: 14,
            ..McpServerInfo::default()
        }
    }

    fn signed(name: &str) -> McpSearchItem {
        McpSearchItem {
            name: name.into(),
            description: "Payments.".into(),
            kinds: "http".into(),
            installed: true,
            tier: McpSourceTier::Vendor,
            installs: Some(9_140),
            signature: McpSignature::Signed,
        }
    }

    fn blocked(name: &str) -> McpSearchItem {
        McpSearchItem {
            name: name.into(),
            description: "Anything at all.".into(),
            kinds: "npm".into(),
            installed: false,
            tier: McpSourceTier::Community,
            installs: Some(112),
            signature: McpSignature::Unsigned,
        }
    }

    fn search_rows(items: Vec<McpSearchItem>) -> Vec<String> {
        let state = McpTabState {
            query: "postgres".into(),
            search: Some(McpSearchOutcome {
                query: "postgres".into(),
                items,
                error: None,
                has_more: false,
            }),
            ..McpTabState::default()
        };
        let mut lines = Vec::new();
        render_search(&state, &mut lines);
        lines.iter().map(flat).collect()
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

    /// **The witness (#5047, pin).** SPEC §9.3: the graph server is pinned
    /// first, with the caption that says why. The old `render_browse` walked
    /// the delivered order, so the graph landed wherever `mcp.toml`'s
    /// alphabetical keys put it — under `github`, and under any alias
    /// starting with a letter before `g`.
    #[test]
    fn the_graph_server_is_pinned_first_and_says_why() {
        let alpha = McpServerInfo {
            name: "aaa".into(),
            granted: true,
            ..stripe()
        };
        let lines = rows(&[alpha, graph(), stripe()]);
        assert!(
            lines[0].contains(GRAPH_SERVER),
            "the graph must head the list whatever order it arrived in: {lines:?}"
        );
        assert!(
            lines[2].contains(PIN_CAPTION),
            "the caption sits with the row it explains: {lines:?}"
        );
        assert!(lines[0].contains("· pinned"), "{lines:?}");
        // Everything else keeps the delivered order underneath the pin.
        assert!(lines[3].contains("aaa"), "{lines:?}");
    }

    /// The pin re-orders the paint, never the selection: `state.selected`
    /// indexes the delivered snapshot, and every key verb acts on that index.
    /// Sorting the rows themselves would make `x` remove whichever server
    /// happened to be painted under the cursor.
    #[test]
    fn pinning_moves_the_paint_and_never_the_selection() {
        let servers = vec![stripe(), graph()];
        assert_eq!(paint_order(&servers), vec![1, 0]);

        let state = McpTabState {
            servers,
            selected: 0,
            ..McpTabState::default()
        };
        assert_eq!(
            state.selected_server().map(|s| s.name.as_str()),
            Some("mcp"),
            "index 0 is still the row the driver delivered first"
        );
        let mut lines = Vec::new();
        render_browse(&state, &mut lines, 80);
        let painted: Vec<String> = lines.iter().map(flat).collect();
        let marked = painted
            .iter()
            .find(|l| l.contains('▸'))
            .unwrap_or_else(|| panic!("nothing is marked selected: {painted:?}"));
        assert!(
            marked.contains("mcp"),
            "the marker follows the selection into its new row: {painted:?}"
        );
    }

    /// **The witness (#5047, latency).** SPEC §9.3's latency column. A
    /// connected server shows its measured round trip; one that is not
    /// connected shows nothing at all, because a stale number beside `not
    /// connected` answers a question nobody asked — and a `0ms` would read as
    /// the nearest server on the list.
    #[test]
    fn a_connected_row_shows_its_latency_and_an_unmeasured_one_shows_none() {
        assert!(
            rows(&[graph()])[0].contains("8ms"),
            "{:?}",
            rows(&[graph()])
        );

        let mut dropped = graph();
        dropped.connected = false;
        dropped.latency_ms = Some(8);
        let text = rows(&[dropped]).join("\n");
        assert!(text.contains("not connected"), "{text}");
        assert!(
            !text.contains("8ms"),
            "a dead connection has no distance: {text}"
        );

        let mut unmeasured = graph();
        unmeasured.latency_ms = None;
        assert!(
            !rows(&[unmeasured])[0].contains("0ms"),
            "unknown is not zero"
        );
    }

    /// **The witness (#5047, registry).** SPEC §9.3: a registry row carries
    /// its source tier, its install count and its signature — and an unsigned
    /// entry is BLOCKED, not merely labelled. The row was
    /// `{name, description, kinds, installed}`, so every one of these was
    /// unanswerable before an operator pressed install.
    #[test]
    fn a_registry_row_carries_tier_installs_and_a_signature_that_can_block() {
        let text = search_rows(vec![signed("com.stripe/mcp")]).join("\n");
        assert!(text.contains("vendor"), "source tier: {text}");
        assert!(text.contains("9.1k installs"), "install count: {text}");
        assert!(text.contains("signed"), "signature: {text}");

        let text = search_rows(vec![blocked("io.github.x/y")]).join("\n");
        assert!(text.contains("community"), "{text}");
        assert!(text.contains("112 installs"), "{text}");
        // The word, not only the colour (SPEC §13).
        assert!(text.contains("unsigned · blocked"), "{text}");
        assert!(text.contains("does not vouch for"), "and why: {text}");
    }

    /// A blocked row refuses the keystroke, and the refusal names the row —
    /// the state the paint shows and the state the key enforces are read from
    /// one value.
    #[test]
    fn install_stands_down_on_a_blocked_row_and_says_so() {
        let state = |item: McpSearchItem| McpTabState {
            query: "q".into(),
            search: Some(McpSearchOutcome {
                query: "q".into(),
                items: vec![item],
                error: None,
                has_more: false,
            }),
            ..McpTabState::default()
        };
        let refusal = state(blocked("io.github.x/y"))
            .selected_search_refusal()
            .expect("a blocked row must refuse");
        assert!(refusal.contains("io.github.x/y"), "{refusal}");
        assert!(refusal.contains("blocked"), "{refusal}");
        assert!(
            state(signed("com.stripe/mcp"))
                .selected_search_refusal()
                .is_none(),
            "a signed row installs"
        );
    }

    /// A registry that publishes no count says so. `0 installs` would claim
    /// nobody runs a server the registry simply does not count.
    #[test]
    fn an_unknown_install_count_is_never_rendered_as_zero() {
        assert_eq!(installs(None), "installs unknown");
        assert_eq!(installs(Some(0)), "0 installs");
        assert_eq!(installs(Some(9_140)), "9.1k installs");
        assert_eq!(installs(Some(2_400_000)), "2.4M installs");
    }

    /// A connected server the model cannot use says so on its row, in words —
    /// otherwise `● 14 tools live` reads as healthy while every call to it is
    /// refused.
    #[test]
    fn an_ungranted_row_says_so_rather_than_reading_as_healthy() {
        let mut ungranted = graph();
        ungranted.granted = false;
        let text = rows(&[ungranted]).join("\n");
        assert!(text.contains("ungranted"), "{text}");
        assert!(rows(&[graph()]).join("\n").contains("live"));
        assert!(!rows(&[graph()]).join("\n").contains("ungranted"));
    }

    #[test]
    fn the_footer_advertises_the_inspector() {
        assert!(flat(&footer(McpMode::Browse)).contains("ctrl+o"));
    }

    /// Read one row of a rendered buffer as text.
    fn row(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width)
            .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
            .collect()
    }

    /// **The witness (#4676).** The key legend is its own band on the floor of
    /// the pane, not the last line of the content paragraph — so it lands on
    /// the same row whether one server is configured or ten. Folded into the
    /// content it rode the list, which is what every sibling pane's port fixed
    /// by giving the legend a `Constraint::Length(1)` of its own.
    #[test]
    fn the_key_row_is_pinned_to_the_floor_whatever_the_list_holds() {
        let area = Rect::new(0, 0, 120, 20);
        let floor = area.height - 1;
        let mut last_row = None;
        for count in [1usize, 6] {
            let mut ui = DeckUi {
                mcp: McpTabState {
                    servers: vec![stripe(); count],
                    status: Some("installed stripe".into()),
                    ..McpTabState::default()
                },
                ..DeckUi::default()
            };
            let mut buf = Buffer::empty(area);
            render(&WorkspaceModel::default(), &mut ui, area, &mut buf);

            assert!(
                row(&buf, floor).contains("ctrl+o inspect"),
                "{count} servers: the legend left the floor:\n{}",
                row(&buf, floor)
            );
            assert!(
                row(&buf, floor - 1).contains("installed stripe"),
                "{count} servers: the driver's last word sits above the legend:\n{}",
                row(&buf, floor - 1)
            );
            // The content band never paints the legend itself.
            let content: String = (0..floor - 1).map(|y| row(&buf, y)).collect();
            assert!(
                !content.contains("ctrl+o"),
                "{count} servers: the legend is still inside the content:\n{content}"
            );
            let painted = row(&buf, floor);
            assert!(
                last_row
                    .replace(painted.clone())
                    .is_none_or(|p| p == painted),
                "the legend moved between list lengths"
            );
        }
    }

    /// The floor row is uncovered by the centered popup, and the popup is
    /// modal — so the pane's verbs come off it while the inspector is up
    /// rather than advertising keys that do nothing.
    #[test]
    fn the_key_row_yields_to_the_modal_inspector() {
        let area = Rect::new(0, 0, 120, 20);
        let mut ui = DeckUi {
            mcp: McpTabState {
                servers: vec![stripe()],
                inspector: Some(McpInspector {
                    server: "mcp".into(),
                    detail: None,
                    scroll: 0,
                }),
                ..McpTabState::default()
            },
            ..DeckUi::default()
        };
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::default(), &mut ui, area, &mut buf);
        let screen: String = (0..area.height).map(|y| row(&buf, y)).collect();
        assert!(
            !screen.contains("ctrl+o inspect"),
            "the inspector is already open; its own verbs are in its title:\n{screen}"
        );
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
            servers: vec![stripe(), graph()],
            query: "stripe".into(),
            search: Some(McpSearchOutcome {
                query: "stripe".into(),
                // One installable row and one blocked one, so the search
                // pane's red arm is painted too.
                items: vec![signed("com.stripe/mcp"), blocked("io.github.x/y")],
                has_more: true,
                error: None,
            }),
            handshake: Some(HandshakeGate {
                server: "mcp".into(),
                detail: Some(McpServerDetail {
                    name: "mcp".into(),
                    connected: true,
                    tools: vec![crate::envelope::McpToolRow {
                        name: "create_refund".into(),
                        description: "Refund a charge.".into(),
                        safe_to_retry: true,
                        calls: 0,
                    }],
                    auth_fields: vec!["Authorization".into()],
                    ..McpServerDetail::default()
                }),
                scroll: 0,
            }),
            ..McpTabState::default()
        };
        for mode in [
            McpMode::Browse,
            McpMode::Search,
            McpMode::Auth,
            McpMode::Handshake,
        ] {
            state.mode = mode;
            let mut lines = Vec::new();
            match mode {
                McpMode::Browse => render_browse(&state, &mut lines, 80),
                McpMode::Search => render_search(&state, &mut lines),
                McpMode::Auth => render_auth(&state, &mut lines),
                McpMode::Handshake => handshake::render(&state, &mut lines, 24),
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
