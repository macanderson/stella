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
//! [`crate::views::frame`] already carved out, so nothing here draws a border,
//! a title bar or a second copy of the tab name.
//!
//! **Stella ships with every tool on.** This pane is how they go off, and it
//! is the only surface that can show an operator what they actually have:
//! MCP tools and customer-registered custom tools exist nowhere but the
//! assembled session stack, so the rows come from the driver
//! ([`crate::envelope::Inbound::ToolPolicy`]), never from a compiled-in table.
//!
//! Ownership mirrors [`crate::views::engine_panel`]: the driver owns the settings
//! files and pushes snapshots; the pane accumulates **unsaved switch edits**
//! and sends them back with [`WorkspaceInput::ToolsSave`]. It sends only the
//! keys it changed. The driver merges them into the chosen scope's own
//! `"tools"` object. A whole-map save would copy the other two scopes'
//! switches into the file being written and freeze them there.
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
//!    they do not have. A launch overlay's denial locks a row the same way
//!    while that launch runs. Its key beats the user and project files, so a
//!    saved switch would stay off and never land. For the same reason, a row
//!    or header whose key the overlay sets refuses to toggle.
//!
//! Interaction is [`crate::views::engine_panel`]'s vocabulary verbatim — modal while
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
use stella_tool_facts::policy::WILDCARD;
use stella_tui_theme::{glyph, token};

use crate::deck::DeckTab;
use crate::deck_ui::{DeckAction, DeckUi, list_nav};
use crate::envelope::{AgentScope, ToolPolicyState, ToolRow, ToolScope, WorkspaceInput};
use crate::render::columns;
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
/// [`stella_tool_facts::policy::ToolPolicy::allows`] enforces, so what the pane
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

/// The settings that locked `tool`: `"launch overlay"` for an overlay's
/// denial, and `"org-managed"` for every other lock.
fn lock_source(tool: &ToolRow) -> &'static str {
    match tool.off.as_ref().and_then(|denial| denial.scope) {
        Some(ToolScope::Overlay) => ToolScope::Overlay.label(),
        _ => ToolScope::Managed.label(),
    }
}

/// The settings that locked every tool in `group`: one source when all the
/// locks share it, and both names when the org locked some and the launch
/// overlay the rest.
fn group_lock_source(state: &ToolPolicyState, group: &str) -> &'static str {
    let mut sources = state
        .tools
        .iter()
        .filter(|tool| tool.group == group && tool.locked)
        .map(lock_source);
    let Some(first) = sources.next() else {
        return ToolScope::Managed.label();
    };
    if sources.all(|source| source == first) {
        first
    } else {
        "org-managed and launch overlay"
    }
}

/// The refusal for a toggle that would write `key` while the launch overlay
/// sets it.
fn overlay_holds(key: &str) -> String {
    format!(
        "\"{key}\" is set by {} settings and cannot be changed here",
        ToolScope::Overlay.label()
    )
}

/// The short tag a locked row or header carries: `org-locked` for the org's
/// ceiling, and `overlay-locked` for a launch overlay's denial.
fn lock_tag(source: &str) -> &'static str {
    if source == ToolScope::Overlay.label() {
        "overlay-locked"
    } else if source == ToolScope::Managed.label() {
        "org-locked"
    } else {
        "locked"
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
    // An edit made before its row locked can't land either: the org or the
    // launch overlay holds that tool off. Nor can an edit to a key the
    // overlay sets. Drop both, so the pane never shows an unsaved change that
    // no save can apply.
    let locked: Vec<&str> = state
        .tools
        .iter()
        .filter(|tool| tool.locked)
        .map(|tool| tool.name.as_str())
        .collect();
    t.edits.retain(|key, _| {
        !locked.contains(&key.as_str()) && !state.overlay_switches.contains_key(key)
    });
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
/// while `ui.tools.focused`. The vocabulary is [`crate::views::engine_panel`]'s, so
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
                    "{} is denied by {} settings — it cannot be switched on here",
                    tool.name,
                    lock_source(tool)
                ));
                return DeckAction::Handled;
            }
            if state.overlay_switches.contains_key(&tool.name) {
                ui.tools.status = Some(overlay_holds(&tool.name));
                return DeckAction::Handled;
            }
            let next = !tool_enabled(&state, &ui.tools.edits, tool);
            ui.tools.edits.insert(tool.name.clone(), next);
        }
        ToolsRow::Group(group) => {
            if group_locked(&state, group) {
                ui.tools.status = Some(format!(
                    "the {group} tools are denied by {} settings — they cannot be switched on \
                     here",
                    group_lock_source(&state, group)
                ));
                return DeckAction::Handled;
            }
            // The header writes the group key. When the overlay sets that key,
            // its value beats the file the save writes, and the edit would
            // never land.
            if state.overlay_switches.contains_key(group) {
                ui.tools.status = Some(overlay_holds(group));
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
    let (off_count, org_locked, overlay_locked, total) = match &t.state {
        None => (0, 0, 0, 0),
        Some(state) => {
            let locked_by = |source: &str| {
                state
                    .tools
                    .iter()
                    .filter(|tool| tool.locked && lock_source(tool) == source)
                    .count()
            };
            (
                state
                    .tools
                    .iter()
                    .filter(|tool| !tool_enabled(state, &t.edits, tool))
                    .count(),
                locked_by(ToolScope::Managed.label()),
                locked_by(ToolScope::Overlay.label()),
                state.tools.len(),
            )
        }
    };

    let mut left = vec![
        Span::styled(
            " tools",
            Style::new().fg(token::TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" · {off_count} off"), muted),
    ];
    if org_locked > 0 {
        left.push(Span::styled(format!(" · {org_locked} org-locked"), muted));
    }
    if overlay_locked > 0 {
        left.push(Span::styled(
            format!(" · {overlay_locked} overlay-locked"),
            muted,
        ));
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
                    // Padded in columns: an MCP server names its own group,
                    // and `{:<width$}` pads to a `char` count, which moves the
                    // state column under any wide glyph.
                    columns::pad(&group.to_uppercase(), NAME_W),
                    Style::new().fg(token::TEXT).add_modifier(Modifier::BOLD),
                ),
                state_span(on),
                Span::styled(format!("{n} {}", plural_tools(n)), muted),
            ];
            if locked {
                let tag = lock_tag(group_lock_source(state, group));
                spans.push(Span::styled(format!(" · {tag}"), muted));
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
                    // Elided one column shorter than the field so a long
                    // namespaced MCP name always keeps a gap before its state,
                    // then padded back out to the field so the state column
                    // holds. Elide and pad are both in display columns: an MCP
                    // name is whatever the server calls itself.
                    format!(
                        "  {}",
                        columns::pad(&columns::head(&tool.name, NAME_W - 3), NAME_W - 2)
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
                    columns::head(&tail, room),
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

/// The `on`/`off` cell. Off is the rare state and takes the muted tone. On
/// is ordinary and reads as text. In a pane where every row is lit, the lit
/// rows say nothing.
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
                        lock_tag(group_lock_source(state, group)).to_string()
                    } else {
                        String::new()
                    },
                ),
            ];
            crate::views::record::record_line(
                crate::views::record::identity(
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
            crate::views::record::record_line(
                crate::views::record::identity(tool.name.clone(), is_sel, theme::INK),
                &fields,
                panel_w,
            )
        }
    }
}

#[cfg(test)]
mod tests;
