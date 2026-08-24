// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The TOOLS pane — the SETTINGS tab's editor for `settings.json` → `tools`:
//! the one map that decides which of this session's tools the agent may use,
//! whether a tool is a built-in, an MCP server's, or one the customer wrote
//! themselves.
//!
//! ```text
//!  tools · 1 off · 1 org-locked · modified                            7 tools
//!  ▸ CUSTOM                    on   1 tool
//!      deploy_to_staging       on
//!    TASK                      on   2 tools
//!      delegate                on
//!      task_assign             off  locked · "task_assign" off in org-managed settings
//!  saved to user settings
//!  ⏎/space toggle · x clear · s save user · S save project · r reload · esc done
//! ```
//!
//! Four bands and no box: a header that carries the counts, the rows, the
//! driver's last word, and the keys. The pane fills the body
//! [`crate::v2::frame`] already carved out, so nothing here draws a border,
//! a title bar or a second copy of the tab name.
//!
//! **Stella ships with every tool on.** This pane is how they go off, and it
//! is the only surface that can show an operator what they actually have:
//! MCP tools and customer-registered custom tools exist nowhere but the
//! assembled session stack, so the rows come from the driver
//! ([`crate::envelope::Inbound::ToolPolicy`]), never from a compiled-in table.
//!
//! Ownership mirrors [`crate::views::engine`]: the driver owns the settings
//! files and pushes snapshots; the pane accumulates **unsaved switch edits**
//! and sends them back with [`WorkspaceInput::ToolsSave`]. What it sends is
//! only the keys it changed — the driver merges them into the chosen scope's
//! own `"tools"` object — because a whole-map save would copy the other two
//! scopes' switches into the file being written and freeze them there.
//!
//! # Two rules
//!
//! 1. **Most specific key wins, and toggling writes the most specific key.**
//!    Toggling one tool writes its exact name, never its group; toggling a
//!    group header writes the group key. A tool can therefore stay off after
//!    its group is switched on (an exact `"save_state": "off"` outranks a
//!    group `"scratch": "on"`) — the row says so, naming the key that did it,
//!    rather than showing a switch that visibly does nothing.
//! 2. **An org-managed denial is not a switch.** A row the managed ceiling
//!    denies renders LOCKED and refuses to toggle. Letting the UI imply a user
//!    can re-enable it would misrepresent the security posture: the write
//!    would be accepted, dropped by [`crate::envelope::AgentScope`]-level
//!    merge on the next load, and the operator would believe they had a tool
//!    they do not have.
//!
//! Interaction is [`crate::views::engine`]'s vocabulary verbatim — modal while
//! focused (`t` on the SETTINGS tab focuses, Esc hands the keyboard back),
//! `⏎`/`space` toggle, `x` clears a row's unsaved edit, `s`/`S` save to the
//! user / project scope, `r` reloads.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use stella_tools::policy::WILDCARD;
use stella_tui_theme::{glyph, token};

use crate::deck::DeckTab;
use crate::deck_ui::{DeckAction, DeckUi, list_nav};
use crate::envelope::{AgentScope, ToolPolicyState, ToolRow, ToolScope, WorkspaceInput};
use crate::render::scroll_window_start;
use crate::theme;
use crate::views::settings::SettingsPane;

/// Hint shown when an action needs the snapshot the driver has not delivered
/// yet (a race right after startup, or a driver error).
const NO_SNAPSHOT_HINT: &str = "waiting for the tool list — r to reload";

/// One line of the pane: a group section header, or one tool under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolsRow {
    /// A section header for one catalog group (`"file"`, `"process"`, and the
    /// `"mcp"` / `"custom"` sections a customer's own tools land in).
    /// Toggling it writes the GROUP key.
    Group(String),
    /// One tool, indexing [`ToolPolicyState::tools`].
    Tool(usize),
}

/// All TOOLS-pane view state (a field on [`DeckUi`]). The switches on disk
/// are driver-owned; `edits` is the unsaved working copy and the only thing a
/// save sends.
#[derive(Debug, Clone, Default)]
pub struct ToolsOverlay {
    /// Whether the pane owns the keyboard (modal while set, on the SETTINGS
    /// tab only — `t` focuses, Esc returns focus to the tab).
    pub focused: bool,
    /// The driver's snapshot. `None` until the first one lands.
    pub state: Option<ToolPolicyState>,
    /// Unsaved switch edits, keyed exactly as they will be written: a tool
    /// name, a group name, or `"*"`. Empty = nothing to save.
    pub edits: BTreeMap<String, bool>,
    /// Selected row, indexing [`ToolsOverlay::rows`].
    pub row: usize,
    /// One-line hint: driver save/refresh outcomes, local refusals.
    pub status: Option<String>,
    /// A save/refresh is in flight driver-side — cleared when the next
    /// [`crate::envelope::Inbound::ToolPolicy`] folds back.
    pub busy: bool,
}

impl ToolsOverlay {
    /// The pane's rows in display order: groups sorted, tools sorted within
    /// each group, one header per group.
    pub fn rows(&self) -> Vec<ToolsRow> {
        self.state.as_ref().map(rows).unwrap_or_default()
    }

    /// Whether there are unsaved switch edits.
    pub fn dirty(&self) -> bool {
        !self.edits.is_empty()
    }
}

/// Group/sort the snapshot into display rows. Pure over the snapshot so the
/// row model can be tested without a terminal.
pub fn rows(state: &ToolPolicyState) -> Vec<ToolsRow> {
    let mut order: Vec<usize> = (0..state.tools.len()).collect();
    order.sort_by(|&a, &b| {
        let (x, y) = (&state.tools[a], &state.tools[b]);
        x.group.cmp(&y.group).then_with(|| x.name.cmp(&y.name))
    });
    let mut rows: Vec<ToolsRow> = Vec::with_capacity(order.len() + 8);
    let mut current: Option<String> = None;
    for i in order {
        let group = &state.tools[i].group;
        if current.as_deref() != Some(group.as_str()) {
            rows.push(ToolsRow::Group(group.clone()));
            current = Some(group.clone());
        }
        rows.push(ToolsRow::Tool(i));
    }
    rows
}

/// Whether `tool` is on right now: the saved switches overlaid with the
/// pane's unsaved edits, resolved most-specific-first — the exact precedence
/// [`stella_tools::policy::ToolPolicy::allows`] enforces, so what the pane
/// shows and what the runtime does cannot disagree.
///
/// A locked row is off, full stop. That short-circuit is the safety property:
/// no local edit, at any level of specificity, may render an org-denied tool
/// as available.
pub fn tool_enabled(
    state: &ToolPolicyState,
    edits: &BTreeMap<String, bool>,
    tool: &ToolRow,
) -> bool {
    if tool.locked {
        return false;
    }
    for key in [tool.name.as_str(), tool.group.as_str(), WILDCARD] {
        if let Some(&value) = edits.get(key) {
            return value;
        }
        if let Some(&value) = state.switches.get(key) {
            return value;
        }
    }
    true
}

/// Whether any tool in `group` is on — what the section header reports. "Any"
/// rather than "all" so a header reads as "this family is usable", and so
/// toggling it off is always the move that changes something.
pub fn group_enabled(state: &ToolPolicyState, edits: &BTreeMap<String, bool>, group: &str) -> bool {
    state
        .tools
        .iter()
        .filter(|tool| tool.group == group)
        .any(|tool| tool_enabled(state, edits, tool))
}

/// Whether the org denies the WHOLE group — the only case where a header is
/// locked. A partially-denied group stays editable: switching it on is a
/// legitimate grant for its unlocked members, and the locked ones keep
/// rendering locked.
pub fn group_locked(state: &ToolPolicyState, group: &str) -> bool {
    let mut members = state
        .tools
        .iter()
        .filter(|tool| tool.group == group)
        .peekable();
    members.peek().is_some() && members.all(|tool| tool.locked)
}

/// The one-line explanation a row carries when it is off under the SAVED
/// settings: the key that did it and the file that key lives in.
pub fn off_reason(tool: &ToolRow) -> Option<String> {
    match (&tool.off, tool.locked) {
        (Some(denial), locked) => {
            let scope = denial.scope.map(ToolScope::label).unwrap_or("settings");
            Some(if locked {
                format!("locked · \"{}\" off in {scope} settings", denial.key)
            } else {
                format!("\"{}\" off in {scope} settings", denial.key)
            })
        }
        // Defensive: a locked row whose denial could not be attributed still
        // says WHY it cannot be edited.
        (None, true) => Some("locked · org-managed settings".to_string()),
        (None, false) => None,
    }
}

// ── driver snapshot ingest ──────────────────────────────────────────────────

/// Fold one [`crate::envelope::Inbound::ToolPolicy`] snapshot.
///
/// The snapshot is always adopted — unlike the engine panel there is no
/// working *copy* to clobber, only a set of deltas, and a delta stays
/// meaningful over a newer base. What the new base does do is retire the
/// deltas it has absorbed: an edit the snapshot now agrees with has landed on
/// disk, so it stops counting as unsaved. That is what clears the "modified"
/// marker exactly when the write actually succeeded — a failed save leaves the
/// edit standing, with the driver's reason on the status line.
pub fn ingest_policy(ui: &mut DeckUi, state: &ToolPolicyState, status: &Option<String>) {
    let t = &mut ui.tools;
    t.edits
        .retain(|key, want| state.switches.get(key) != Some(&*want));
    t.state = Some(state.clone());
    if let Some(status) = status {
        t.status = Some(status.clone());
    }
    t.busy = false;
}

// ── focus (`t` on the SETTINGS tab) ────────────────────────────────────────

/// Focus the TOOLS pane (switching to the SETTINGS tab if needed) and ask the
/// driver to re-enumerate the session's tools and re-read the settings chain.
/// The engine panel gives up the keyboard: the SETTINGS tab hosts two editors,
/// and exactly one of them is modal at a time.
pub fn focus_panel(ui: &mut DeckUi) -> DeckAction {
    ui.set_tab(DeckTab::Settings);
    ui.engine.focused = false;
    // The tab shows one pane at a time — move the nav with the focus so the
    // editor holding the keyboard is always the one on screen.
    ui.settings_pane = SettingsPane::Tools;
    let t = &mut ui.tools;
    t.focused = true;
    t.row = 0;
    t.busy = true;
    DeckAction::Send(WorkspaceInput::ToolsRefresh)
}

// ── key handling ────────────────────────────────────────────────────────────

/// The pane's modal key map, dispatched by [`crate::deck_ui::handle_deck_key`]
/// while `ui.tools.focused`. The vocabulary is [`crate::views::engine`]'s, so
/// the two editors on one tab never need two things learned.
pub fn handle_tools_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    let plain = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META);
    // Row movement is the deck's one vocabulary, not this pane's: `↑`/`↓`,
    // `j`/`k`, `⇞`/`⇟` and `Home`/`End` all move the selection. `letters` is
    // true because the pane is modal while focused — nothing here is
    // composing a prompt for `j` to join (#4370).
    let count = ui.tools.rows().len();
    if list_nav::select(key, &mut ui.tools.row, count, true) {
        return DeckAction::Handled;
    }
    match key.code {
        KeyCode::Esc => {
            ui.tools.focused = false;
            DeckAction::Handled
        }
        KeyCode::Enter => toggle_row(ui),
        KeyCode::Char(' ') if plain => toggle_row(ui),
        KeyCode::Char('x') if plain => clear_row(ui),
        KeyCode::Char('s') if plain => save(ui, AgentScope::User),
        KeyCode::Char('S') if plain => save(ui, AgentScope::Project),
        KeyCode::Char('r') if plain => refresh(ui),
        // Modal: swallow everything else so no verb leaks into the composer.
        _ => DeckAction::Handled,
    }
}

/// `⏎`/`space`: flip the selected row, writing the MOST SPECIFIC key — the
/// exact tool name for a tool row, the group name for a header.
fn toggle_row(ui: &mut DeckUi) -> DeckAction {
    let Some(state) = ui.tools.state.clone() else {
        ui.tools.status = Some(NO_SNAPSHOT_HINT.into());
        return DeckAction::Handled;
    };
    let rows = rows(&state);
    let Some(row) = rows.get(ui.tools.row.min(rows.len().saturating_sub(1))) else {
        return DeckAction::Handled;
    };
    match row {
        ToolsRow::Tool(i) => {
            let tool = &state.tools[*i];
            if tool.locked {
                ui.tools.status = Some(format!(
                    "{} is denied by org-managed settings — it cannot be switched on here",
                    tool.name
                ));
                return DeckAction::Handled;
            }
            let next = !tool_enabled(&state, &ui.tools.edits, tool);
            ui.tools.edits.insert(tool.name.clone(), next);
        }
        ToolsRow::Group(group) => {
            if group_locked(&state, group) {
                ui.tools.status = Some(format!(
                    "the {group} tools are denied by org-managed settings — they cannot be \
                     switched on here"
                ));
                return DeckAction::Handled;
            }
            let next = !group_enabled(&state, &ui.tools.edits, group);
            // Member-level edits are dropped first: they are more specific and
            // would outrank the header the user just used, making it look
            // broken. Member-level keys already SAVED still outrank it — the
            // row keeps reporting the key that did it, rather than silently
            // rewriting settings the user did not select.
            let members: Vec<String> = state
                .tools
                .iter()
                .filter(|tool| &tool.group == group)
                .map(|tool| tool.name.clone())
                .collect();
            for name in members {
                ui.tools.edits.remove(&name);
            }
            ui.tools.edits.insert(group.clone(), next);
        }
    }
    DeckAction::Handled
}

/// `x`: drop the selected row's unsaved edit, returning it to whatever the
/// saved settings say. Never writes a switch — clearing an edit is not the
/// same as switching a tool on.
fn clear_row(ui: &mut DeckUi) -> DeckAction {
    let Some(state) = ui.tools.state.clone() else {
        ui.tools.status = Some(NO_SNAPSHOT_HINT.into());
        return DeckAction::Handled;
    };
    let rows = rows(&state);
    match rows.get(ui.tools.row.min(rows.len().saturating_sub(1))) {
        Some(ToolsRow::Tool(i)) => {
            ui.tools.edits.remove(&state.tools[*i].name);
        }
        Some(ToolsRow::Group(group)) => {
            ui.tools.edits.remove(group);
        }
        None => {}
    }
    DeckAction::Handled
}

/// `s`/`S`: send the unsaved edits to the driver for persistence at `scope`.
/// The reply — a fresh snapshot with the outcome in `status` — clears `busy`
/// and retires the edits the write landed ([`ingest_policy`]).
fn save(ui: &mut DeckUi, scope: AgentScope) -> DeckAction {
    if ui.tools.edits.is_empty() {
        ui.tools.status = Some("no switch changes to save".into());
        return DeckAction::Handled;
    }
    let switches = ui.tools.edits.clone();
    ui.tools.busy = true;
    ui.tools.status = Some(format!("saving to {} settings…", scope.label()));
    ui.pending_inputs
        .push(WorkspaceInput::ToolsSave { switches, scope });
    DeckAction::Handled
}

/// `r`: ask the driver to re-enumerate the session's tools and re-read the
/// settings chain.
fn refresh(ui: &mut DeckUi) -> DeckAction {
    ui.tools.busy = true;
    ui.tools.status = Some("reloading tool switches…".into());
    ui.pending_inputs.push(WorkspaceInput::ToolsRefresh);
    DeckAction::Handled
}

// ── render ──────────────────────────────────────────────────────────────────

/// Name column width — fits a namespaced MCP tool's head without pushing the
/// on/off state off a narrow pane.
const NAME_W: usize = 26;

/// Draw the pane into the body the frame carved out: header, rows, the
/// driver's last word, keys.
pub fn render_panel(ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    let t = &ui.tools;
    if area.width < 4 || area.height == 0 {
        return; // no readable pane fits — draw nothing rather than garbage
    }
    let bands = Layout::vertical([
        Constraint::Length(1), // header · counts
        Constraint::Min(0),    // rows
        Constraint::Length(1), // the driver's last word
        Constraint::Length(1), // keys
    ])
    .split(area);

    render_header(t, bands[0], buf);
    render_rows(ui, bands[1], buf);
    render_status(t, bands[2], buf);
    render_keys(t, bands[3], buf);
}

/// `tools · 1 off · 1 org-locked · modified` with the session's tool count on
/// the right edge — the counts the bordered title used to carry, on a row
/// that costs the same one line and can hold the total as well.
fn render_header(t: &ToolsOverlay, area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let muted = Style::new().fg(token::MUTED);
    let (off_count, locked_count, total) = match &t.state {
        None => (0, 0, 0),
        Some(state) => (
            state
                .tools
                .iter()
                .filter(|tool| !tool_enabled(state, &t.edits, tool))
                .count(),
            state.tools.iter().filter(|tool| tool.locked).count(),
            state.tools.len(),
        ),
    };

    let mut left = vec![
        Span::styled(
            " tools",
            Style::new().fg(token::TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" · {off_count} off"), muted),
    ];
    if locked_count > 0 {
        left.push(Span::styled(format!(" · {locked_count} org-locked"), muted));
    }
    if t.dirty() {
        // The one thing on the pane asking to be acted on, so it takes the
        // accent and nothing else here does.
        left.push(Span::styled(" · modified", Style::new().fg(token::GOLD)));
    }

    let right = format!("{total} {} ", plural_tools(total));
    let used: usize = left.iter().map(Span::width).sum();
    let width = area.width as usize;
    if total > 0 && used + right.chars().count() < width {
        left.push(Span::raw(" ".repeat(width - used - right.chars().count())));
        left.push(Span::styled(right, Style::new().fg(token::DIM)));
    }
    Paragraph::new(Line::from(left)).render(area, buf);
}

/// The windowed row list, or the one line that says why there is none.
fn render_rows(ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let t = &ui.tools;
    let muted = Style::new().fg(token::MUTED);
    let visible = area.height as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();

    match &t.state {
        None => lines.push(Line::from(Span::styled(
            format!("  {NO_SNAPSHOT_HINT}"),
            muted,
        ))),
        Some(state) => {
            let rows = t.rows();
            let count = rows.len();
            if count == 0 {
                lines.push(Line::from(Span::styled(
                    "  no tools in this session yet — r to reload",
                    muted,
                )));
            }
            let sel = t.row.min(count.saturating_sub(1));
            let first = scroll_window_start(count, sel, visible);
            let last = (first + visible).min(count);
            for (i, row) in rows.iter().enumerate().take(last).skip(first) {
                lines.push(if ui.accessible {
                    row_record(t, state, row, i == sel, area.width as usize)
                } else {
                    render_row(t, state, row, i == sel, area.width as usize)
                });
            }
        }
    }
    Paragraph::new(lines).render(area, buf);
}

/// The driver's last word on a save or a refresh, or the local refusal that
/// replaced it. Blank while there is nothing to say.
fn render_status(t: &ToolsOverlay, area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let Some(status) = t
        .status
        .clone()
        .or_else(|| t.busy.then(|| "working…".to_string()))
    else {
        return;
    };
    Paragraph::new(Line::from(Span::styled(
        format!(" {status}"),
        Style::new().fg(token::GOLD),
    )))
    .render(area, buf);
}

/// The key row: the pane's verbs while it holds the keyboard, and the one key
/// that takes it otherwise.
///
/// `e` edits whichever pane the SETTINGS nav is on, so the hint on the visible
/// pane is `e` — not the pane-specific `t`, which survives only as an
/// accelerator from the other pane.
fn render_keys(t: &ToolsOverlay, area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let key = Style::new().fg(token::MUTED);
    let dim = Style::new().fg(token::DIM);
    let verbs: &[(&str, &str)] = if t.focused {
        &[
            ("⏎/space", "toggle"),
            ("x", "clear"),
            ("s", "save user"),
            ("S", "save project"),
            ("r", "reload"),
            ("esc", "done"),
        ]
    } else {
        &[("e", "edit tool switches")]
    };
    let mut spans = vec![Span::raw(" ")];
    for (i, (chord, label)) in verbs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(Span::styled((*chord).to_string(), key));
        spans.push(Span::styled(format!(" {label}"), dim));
    }
    Paragraph::new(Line::from(spans)).render(area, buf);
}

/// One pane row: a group header, or `▸ name  on|off  reason`.
///
/// The selected row takes the highlight ground and the caret; the caret keeps
/// its two columns either way, so the name column never shifts under a moving
/// selection.
fn render_row(
    t: &ToolsOverlay,
    state: &ToolPolicyState,
    row: &ToolsRow,
    is_sel: bool,
    panel_w: usize,
) -> Line<'static> {
    let muted = Style::new().fg(token::MUTED);
    let caret = Span::styled(
        if is_sel {
            format!(" {} ", glyph::COLLAPSED)
        } else {
            "   ".to_string()
        },
        Style::new().fg(token::GOLD),
    );
    let mut line = match row {
        ToolsRow::Group(group) => {
            let on = group_enabled(state, &t.edits, group);
            let locked = group_locked(state, group);
            let n = state
                .tools
                .iter()
                .filter(|tool| &tool.group == group)
                .count();
            let mut spans = vec![
                caret,
                Span::styled(
                    format!("{:<width$}", group.to_uppercase(), width = NAME_W),
                    Style::new().fg(token::TEXT).add_modifier(Modifier::BOLD),
                ),
                state_span(on),
                Span::styled(format!("{n} {}", plural_tools(n)), muted),
            ];
            if locked {
                spans.push(Span::styled(" · org-locked".to_string(), muted));
            }
            Line::from(spans)
        }
        ToolsRow::Tool(i) => {
            let tool = &state.tools[*i];
            let on = tool_enabled(state, &t.edits, tool);
            let pending = t.edits.contains_key(&tool.name);
            let mut spans = vec![
                caret,
                Span::styled(
                    // Truncated one char shorter than the column so a long
                    // namespaced MCP name always keeps a gap before its state.
                    format!(
                        "  {:<width$}",
                        truncate_chars(&tool.name, NAME_W - 3),
                        width = NAME_W - 2
                    ),
                    Style::new().fg(token::SILVER),
                ),
                state_span(on),
            ];
            // While an edit is pending the saved reason is stale — say
            // "unsaved" instead of a sentence that describes disk.
            let tail = if pending {
                Some("unsaved".to_string())
            } else {
                off_reason(tool)
            };
            if let Some(tail) = tail {
                let room = panel_w.saturating_sub(3 + 2 + NAME_W + 5 + 1).max(8);
                spans.push(Span::styled(
                    truncate_chars(&tail, room),
                    Style::new().fg(token::DIM),
                ));
            }
            Line::from(spans)
        }
    };
    if is_sel {
        line.style = Style::new().bg(token::HL).add_modifier(Modifier::BOLD);
    }
    line
}

/// The `on`/`off` cell. Off is the exceptional state and takes the muted tone;
/// on is ordinary and reads as text, because a pane where every row is lit is
/// a pane where the lit rows say nothing.
fn state_span(on: bool) -> Span<'static> {
    Span::styled(
        format!("{:<5}", if on { "on" } else { "off" }),
        Style::new().fg(if on { token::TEXT } else { token::MUTED }),
    )
}

/// `tool`/`tools` for a count.
fn plural_tools(n: usize) -> &'static str {
    if n == 1 { "tool" } else { "tools" }
}

/// One TOOLS row in accessible mode: the same fields, each named, with no
/// column padding.
///
/// The default row is three fixed-width columns — name, then `on`/`off`, then
/// a reason — and the padding between them is what separates the values. A
/// reader collapses runs of spaces, so `get_state            on` becomes
/// `get_state on`, which happens to still be readable; `mcp__github__create…
/// off  org policy` does not, because nothing says which token is the state
/// and which is the reason. Both get labels here rather than only the row that
/// visibly breaks.
fn row_record(
    t: &ToolsOverlay,
    state: &ToolPolicyState,
    row: &ToolsRow,
    is_sel: bool,
    panel_w: usize,
) -> Line<'static> {
    match row {
        ToolsRow::Group(group) => {
            let on = group_enabled(state, &t.edits, group);
            let count = state
                .tools
                .iter()
                .filter(|tool| &tool.group == group)
                .count();
            let fields = [
                ("state", if on { "on" } else { "off" }.to_string()),
                ("tools", count.to_string()),
                (
                    "policy",
                    if group_locked(state, group) {
                        "org-locked".to_string()
                    } else {
                        String::new()
                    },
                ),
            ];
            crate::views::linear::record_line(
                crate::views::linear::identity(
                    format!("group {}", group.to_uppercase()),
                    is_sel,
                    theme::ACCENT,
                ),
                &fields,
                panel_w,
            )
        }
        ToolsRow::Tool(i) => {
            let tool = &state.tools[*i];
            let on = tool_enabled(state, &t.edits, tool);
            // While an edit is pending the saved reason is stale — say
            // "unsaved" instead of a sentence that describes disk.
            let reason = if t.edits.contains_key(&tool.name) {
                Some("unsaved".to_string())
            } else {
                off_reason(tool)
            };
            let fields = [
                ("state", if on { "on" } else { "off" }.to_string()),
                ("group", tool.group.clone()),
                ("why", reason.unwrap_or_default()),
            ];
            crate::views::linear::record_line(
                crate::views::linear::identity(tool.name.clone(), is_sel, theme::INK),
                &fields,
                panel_w,
            )
        }
    }
}

/// Char-safe prefix truncation with an ellipsis — a namespaced MCP tool name
/// must never wrap the row.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::WorkspaceModel;
    use crate::deck_ui::{handle_deck_key, ingest_inbound};
    use crate::envelope::{Inbound, ToolDenial};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    fn tool(name: &str, group: &str) -> ToolRow {
        ToolRow {
            name: name.into(),
            group: group.into(),
            locked: false,
            off: None,
        }
    }

    /// A session with the built-in families, an MCP server's tool, and a
    /// customer's own registered tool — the three sources the pane must show.
    fn sample_state() -> ToolPolicyState {
        ToolPolicyState {
            tools: vec![
                tool("get_environment", "environment"),
                tool("delegate", "task"),
                tool("get_state", "scratch"),
                tool("save_state", "scratch"),
                tool("mcp__gh__create_issue", "mcp"),
                tool("deploy_to_staging", "custom"),
            ],
            switches: BTreeMap::new(),
        }
    }

    fn open_ui() -> (WorkspaceModel, DeckUi) {
        let model = WorkspaceModel::new();
        let mut ui = DeckUi::default();
        ui.splash.skip();
        ui.set_tab(DeckTab::Settings);
        ui.tools.focused = true;
        ui.tools.state = Some(sample_state());
        (model, ui)
    }

    #[test]
    fn rows_group_every_source_including_mcp_and_custom_tools() {
        let state = sample_state();
        let labels: Vec<String> = rows(&state)
            .into_iter()
            .map(|row| match row {
                ToolsRow::Group(g) => format!("[{g}]"),
                ToolsRow::Tool(i) => state.tools[i].name.clone(),
            })
            .collect();
        assert_eq!(
            labels,
            vec![
                "[custom]".to_string(),
                // A customer's own tool gets its own section, by name.
                "deploy_to_staging".to_string(),
                "[environment]".to_string(),
                "get_environment".to_string(),
                "[mcp]".to_string(),
                "mcp__gh__create_issue".to_string(),
                "[scratch]".to_string(),
                // Sorted within the group.
                "get_state".to_string(),
                "save_state".to_string(),
                "[task]".to_string(),
                "delegate".to_string(),
            ],
            "groups sorted, tools sorted within them, one header each"
        );
    }

    #[test]
    fn a_row_resolves_most_specific_key_first_over_saved_switches_and_edits() {
        let mut state = sample_state();
        state.switches.insert(WILDCARD.into(), false);
        state.switches.insert("scratch".into(), true);
        state.switches.insert("save_state".into(), false);
        let none = BTreeMap::new();

        let at = |name: &str| {
            state
                .tools
                .iter()
                .find(|t| t.name == name)
                .expect("tool present")
        };
        assert!(
            !tool_enabled(&state, &none, at("get_environment")),
            "wildcard off"
        );
        assert!(
            tool_enabled(&state, &none, at("get_state")),
            "group on beats the wildcard"
        );
        assert!(
            !tool_enabled(&state, &none, at("save_state")),
            "exact off beats the group"
        );

        // An edit at a LESS specific level must not defeat a saved exact key —
        // the pane would otherwise show a tool as on that the runtime keeps off.
        let mut edits = BTreeMap::new();
        edits.insert("scratch".to_string(), true);
        assert!(
            !tool_enabled(&state, &edits, at("save_state")),
            "a pending group grant does not outrank a saved exact denial"
        );
        // …and an edit at the SAME level does.
        edits.insert("save_state".to_string(), true);
        assert!(tool_enabled(&state, &edits, at("save_state")));
    }

    /// Deliberately over `save_state`, whose group (`scratch`) has a
    /// DIFFERENT name. No catalog row is named after its own group any more
    /// (#3192), so every row would now distinguish the two — but a row whose
    /// name and group matched would hide the bug, and that is the shape this
    /// test is written against.
    #[test]
    fn toggling_a_tool_writes_its_exact_name_never_its_group() {
        let (model, mut ui) = open_ui();
        let rows = ui.tools.rows();
        let state = ui.tools.state.clone().unwrap();
        ui.tools.row = rows
            .iter()
            .position(
                |row| matches!(row, ToolsRow::Tool(i) if state.tools[*i].name == "save_state"),
            )
            .expect("save_state row");

        handle_deck_key(ch(' '), &model, &mut ui);
        assert_eq!(
            ui.tools.edits,
            BTreeMap::from([("save_state".to_string(), false)]),
            "the exact tool key, and only it — never the `scratch` group"
        );
        // Its sibling in the same group is untouched, which is the whole point
        // of writing the most specific key.
        let get_state = state.tools.iter().find(|t| t.name == "get_state").unwrap();
        assert!(tool_enabled(&state, &ui.tools.edits, get_state));

        // Toggling back flips the same key rather than deleting it.
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        assert_eq!(
            ui.tools.edits,
            BTreeMap::from([("save_state".to_string(), true)])
        );
        // `x` drops the unsaved edit entirely.
        handle_deck_key(ch('x'), &model, &mut ui);
        assert!(ui.tools.edits.is_empty(), "x clears the pending edit");
        assert!(!ui.tools.dirty());
    }

    #[test]
    fn toggling_a_group_header_writes_the_group_key() {
        let (model, mut ui) = open_ui();
        let rows = ui.tools.rows();
        ui.tools.row = rows
            .iter()
            .position(|row| matches!(row, ToolsRow::Group(g) if g == "scratch"))
            .expect("scratch header");
        handle_deck_key(ch(' '), &model, &mut ui);
        assert_eq!(
            ui.tools.edits,
            BTreeMap::from([("scratch".to_string(), false)]),
            "the group key covers the family in one line"
        );
        let state = ui.tools.state.clone().unwrap();
        for name in ["get_state", "save_state"] {
            let tool = state.tools.iter().find(|t| t.name == name).unwrap();
            assert!(
                !tool_enabled(&state, &ui.tools.edits, tool),
                "{name} is off"
            );
        }
        // A member edit made afterwards is more specific and wins.
        ui.tools.row = rows
            .iter()
            .position(|row| matches!(row, ToolsRow::Tool(i) if state.tools[*i].name == "get_state"))
            .expect("get_state row");
        handle_deck_key(ch(' '), &model, &mut ui);
        assert_eq!(ui.tools.edits.get("get_state"), Some(&true));
    }

    #[test]
    fn a_managed_denied_row_is_locked_and_refuses_to_toggle_on() {
        let (model, mut ui) = open_ui();
        let mut state = sample_state();
        state.switches.insert("delegate".into(), false);
        for tool in &mut state.tools {
            if tool.name == "delegate" {
                tool.locked = true;
                tool.off = Some(ToolDenial {
                    key: "delegate".into(),
                    scope: Some(ToolScope::Managed),
                });
            }
        }
        ui.tools.state = Some(state);
        let rows = ui.tools.rows();
        let state_snapshot = ui.tools.state.clone().unwrap();
        ui.tools.row = rows
            .iter()
            .position(
                |row| matches!(row, ToolsRow::Tool(i) if state_snapshot.tools[*i].name == "delegate"),
            )
            .expect("the `delegate` tool row");

        handle_deck_key(ch(' '), &model, &mut ui);
        assert!(
            ui.tools.edits.is_empty(),
            "a locked row must not produce a switch the org will drop"
        );
        assert!(
            ui.tools
                .status
                .as_deref()
                .is_some_and(|s| s.contains("org-managed")),
            "the refusal says why: {:?}",
            ui.tools.status
        );
        let state = ui.tools.state.as_ref().unwrap();
        let delegate = state.tools.iter().find(|t| t.name == "delegate").unwrap();
        assert!(!tool_enabled(state, &ui.tools.edits, delegate));
        assert_eq!(
            off_reason(delegate).as_deref(),
            Some("locked · \"delegate\" off in org-managed settings")
        );

        // Even a forged edit (a group or wildcard grant reaching the map any
        // other way) cannot render it on.
        let mut edits = BTreeMap::new();
        edits.insert(WILDCARD.to_string(), true);
        edits.insert("delegate".to_string(), true);
        assert!(
            !tool_enabled(state, &edits, delegate),
            "locked short-circuits every level of the precedence ladder"
        );
        // The group header the org fully denies is locked too.
        assert!(group_locked(state, "task"));
    }

    #[test]
    fn s_and_shift_s_send_only_the_edited_keys_at_the_chosen_scope() {
        let (model, mut ui) = open_ui();
        ui.tools.row = 1; // deploy_to_staging, the first tool row
        handle_deck_key(ch(' '), &model, &mut ui);

        handle_deck_key(ch('s'), &model, &mut ui);
        assert_eq!(
            ui.pending_inputs,
            vec![WorkspaceInput::ToolsSave {
                switches: BTreeMap::from([("deploy_to_staging".to_string(), false)]),
                scope: AgentScope::User,
            }],
            "only the changed key goes out, at user scope"
        );
        assert!(ui.tools.busy);

        ui.pending_inputs.clear();
        handle_deck_key(ch('S'), &model, &mut ui);
        assert_eq!(
            ui.pending_inputs,
            vec![WorkspaceInput::ToolsSave {
                switches: BTreeMap::from([("deploy_to_staging".to_string(), false)]),
                scope: AgentScope::Project,
            }]
        );
    }

    #[test]
    fn the_save_echo_retires_the_edit_and_a_failure_keeps_it() {
        let mut model = WorkspaceModel::new();
        let mut ui = DeckUi::default();
        ui.splash.skip();
        ui.tools.state = Some(sample_state());
        ui.tools.edits.insert("delegate".into(), false);
        ui.tools.busy = true;

        // A failed save: the snapshot still says `delegate` is on, so the edit
        // stands.
        ingest_inbound(
            &Inbound::ToolPolicy {
                state: sample_state(),
                status: Some("save failed: cannot read".into()),
            },
            &mut model,
            &mut ui,
        );
        assert_eq!(
            ui.tools.edits.get("delegate"),
            Some(&false),
            "still unsaved"
        );
        assert!(!ui.tools.busy, "a snapshot always ends the in-flight op");
        assert!(ui.tools.dirty());

        // The successful echo carries the switch — the marker clears.
        let mut saved = sample_state();
        saved.switches.insert("delegate".into(), false);
        ingest_inbound(
            &Inbound::ToolPolicy {
                state: saved.clone(),
                status: Some("saved to user settings".into()),
            },
            &mut model,
            &mut ui,
        );
        assert!(ui.tools.edits.is_empty(), "the write landed");
        assert!(!ui.tools.dirty());
        assert_eq!(ui.tools.state.as_ref(), Some(&saved));
        assert_eq!(ui.tools.status.as_deref(), Some("saved to user settings"));
    }

    #[test]
    fn t_on_the_settings_tab_focuses_the_panel_and_esc_releases_it() {
        let model = WorkspaceModel::new();
        let mut ui = DeckUi::default();
        ui.splash.skip();
        ui.set_tab(DeckTab::Settings);

        let action = handle_deck_key(ch('t'), &model, &mut ui);
        assert_eq!(
            action,
            DeckAction::Send(WorkspaceInput::ToolsRefresh),
            "focusing asks the driver for the live tool list"
        );
        assert!(ui.tools.focused);

        handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
        assert!(!ui.tools.focused, "esc hands the keyboard back to the tab");
        // `e` then reaches the engine panel, which takes the keyboard from the
        // tools pane — one editor owns the SETTINGS keyboard at a time.
        handle_deck_key(ch('t'), &model, &mut ui);
        crate::views::engine::focus_panel(&mut ui);
        assert!(ui.engine.focused);
        assert!(
            !ui.tools.focused,
            "focusing the engine releases the tools pane"
        );
        focus_panel(&mut ui);
        assert!(ui.tools.focused);
        assert!(!ui.engine.focused, "and the other way around");
    }

    /// While one editor is modal, the other's focus key is swallowed rather
    /// than leaking into the composer behind the pane — the same rule every
    /// modal surface on the deck follows.
    #[test]
    fn a_focused_editor_swallows_the_other_editors_focus_key() {
        let (model, mut ui) = open_ui();
        let action = handle_deck_key(ch('e'), &model, &mut ui);
        assert_eq!(action, DeckAction::Handled);
        assert!(!ui.engine.focused, "e did not reach the engine panel");
        assert!(ui.tools.focused);
        assert!(ui.composer.is_empty(), "and nothing reached the composer");
    }

    #[test]
    fn the_reason_names_the_key_and_the_scope_that_switched_it_off() {
        let mut off = tool("save_state", "scratch");
        off.off = Some(ToolDenial {
            key: "scratch".into(),
            scope: Some(ToolScope::Project),
        });
        assert_eq!(
            off_reason(&off).as_deref(),
            Some("\"scratch\" off in project settings")
        );
        assert_eq!(off_reason(&tool("get_state", "scratch")), None);
    }

    fn buffer_text(buf: &Buffer) -> String {
        let area = buf.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn drawn(ui: &DeckUi, w: u16, h: u16) -> String {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        render_panel(ui, area, &mut buf);
        buffer_text(&buf)
    }

    fn locked_ui() -> DeckUi {
        let (_model, mut ui) = open_ui();
        let mut state = sample_state();
        state.switches.insert("scratch".into(), false);
        for tool in &mut state.tools {
            if tool.group == "scratch" {
                tool.off = Some(ToolDenial {
                    key: "scratch".into(),
                    scope: Some(ToolScope::User),
                });
            }
            if tool.name == "delegate" {
                tool.locked = true;
                tool.off = Some(ToolDenial {
                    key: "delegate".into(),
                    scope: Some(ToolScope::Managed),
                });
            }
        }
        ui.tools.state = Some(state);
        ui
    }

    #[test]
    fn render_smoke_draws_groups_states_and_reasons() {
        let ui = locked_ui();
        let text = drawn(&ui, 96, 24);
        assert!(text.contains("tools ·"), "header drawn");
        assert!(text.contains("SCRATCH"), "group headers drawn");
        assert!(text.contains("MCP"), "an MCP section is listed");
        assert!(text.contains("CUSTOM"), "a custom-tool section is listed");
        assert!(
            text.contains("deploy_to_staging"),
            "the customer's own tool"
        );
        assert!(
            text.contains("off in user settings"),
            "an off row explains itself"
        );
        assert!(text.contains("locked"), "an org-denied row says so");
        assert!(text.contains("org-locked"), "the header counts locked rows");
    }

    /// **The witness for the port.** The pane fills the body the frame carved
    /// out and draws no box of its own: the counts are a header row and the
    /// verbs are a key row, so nothing spends a column or a row on a border
    /// that repeats what the tab strip already said.
    #[test]
    fn the_pane_draws_no_box_of_its_own() {
        let ui = locked_ui();
        let text = drawn(&ui, 96, 24);
        for edge in ['┌', '┐', '└', '┘', '│', '─', '╭', '╮', '╰', '╯'] {
            assert!(
                !text.contains(edge),
                "the pane drew a border glyph {edge:?}:\n{text}"
            );
        }
        let first = text.lines().next().unwrap_or_default();
        assert!(
            first.trim_start().starts_with("tools ·"),
            "the header is the first row, not a border: {first:?}"
        );
        assert!(
            first.contains("6 tools"),
            "the header carries the session's tool count: {first:?}"
        );
    }

    /// The keys are the pane's last row either way, and they name what the
    /// keyboard actually does right now — the verbs while it is focused, the
    /// one key that focuses it while it is not.
    #[test]
    fn the_key_row_follows_the_keyboard() {
        let mut ui = locked_ui();
        let focused = drawn(&ui, 96, 24);
        let last = focused.lines().last().unwrap_or_default().to_string();
        assert!(last.contains("⏎/space toggle"), "{last:?}");
        assert!(last.contains("esc done"), "{last:?}");

        ui.tools.focused = false;
        let browsing = drawn(&ui, 96, 24);
        let last = browsing.lines().last().unwrap_or_default().to_string();
        assert_eq!(last.trim_end(), " e edit tool switches");
    }

    /// A pane too short for its own bands still draws what fits rather than
    /// panicking or painting outside the body.
    #[test]
    fn a_short_pane_still_renders() {
        let ui = locked_ui();
        for h in 1..=4u16 {
            let text = drawn(&ui, 96, h);
            assert_eq!(text.lines().count(), h as usize);
        }
        // Narrower than the caret plus a name is not a pane at all.
        assert_eq!(drawn(&ui, 3, 24).trim(), "");
    }

    #[test]
    fn the_modified_marker_appears_only_with_unsaved_edits() {
        let mut ui = locked_ui();
        assert!(!drawn(&ui, 96, 24).contains("modified"));
        ui.tools.edits.insert("get_state".into(), false);
        assert!(drawn(&ui, 96, 24).contains("· modified"));
    }
}
