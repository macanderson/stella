//! The TOOLS panel — the SETTINGS tab's editor for `settings.json` →
//! `tools`: the one map that decides which of this session's tools the agent
//! may use, whether a tool is a built-in, an MCP server's, or one the customer
//! wrote themselves.
//!
//! **Stella ships with every tool on.** This panel is how they go off, and it
//! is the only surface that can show an operator what they actually have:
//! MCP tools and customer-registered custom tools exist nowhere but the
//! assembled session stack, so the rows come from the driver
//! ([`crate::envelope::Inbound::ToolPolicy`]), never from a compiled-in table.
//!
//! Ownership mirrors [`crate::views::engine`]: the driver owns the settings
//! files and pushes snapshots; the panel accumulates **unsaved switch edits**
//! and sends them back with [`WorkspaceInput::ToolsSave`]. What it sends is
//! only the keys it changed — the driver merges them into the chosen scope's
//! own `"tools"` object — because a whole-map save would copy the other two
//! scopes' switches into the file being written and freeze them there.
//!
//! # Two invariants worth stating
//!
//! 1. **Most specific key wins, and toggling writes the most specific key.**
//!    Toggling one tool writes its exact name, never its group; toggling a
//!    group header writes the group key. A tool can therefore stay off after
//!    its group is switched on (an exact `"send_stdin": "off"` outranks a
//!    group `"process": "on"`) — the row says so, naming the key that did it,
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
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use stella_tools::policy::WILDCARD;

use crate::deck::DeckTab;
use crate::deck_ui::{DeckAction, DeckUi};
use crate::envelope::{AgentScope, ToolPolicyState, ToolRow, ToolScope, WorkspaceInput};
use crate::render::scroll_window_start;
use crate::theme;

/// Hint shown when an action needs the snapshot the driver has not delivered
/// yet (a race right after startup, or a driver error).
const NO_SNAPSHOT_HINT: &str = "waiting for the tool list — r to reload";

/// One line of the panel: a group section header, or one tool under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolsRow {
    /// A section header for one catalog group (`"file"`, `"process"`, and the
    /// `"mcp"` / `"custom"` sections a customer's own tools land in).
    /// Toggling it writes the GROUP key.
    Group(String),
    /// One tool, indexing [`ToolPolicyState::tools`].
    Tool(usize),
}

/// All TOOLS-panel view state (a field on [`DeckUi`]). The switches on disk
/// are driver-owned; `edits` is the unsaved working copy and the only thing a
/// save sends.
#[derive(Debug, Clone, Default)]
pub struct ToolsOverlay {
    /// Whether the panel owns the keyboard (modal while set, on the SETTINGS
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
    /// The panel's rows in display order: groups sorted, tools sorted within
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
/// panel's unsaved edits, resolved most-specific-first — the exact precedence
/// [`stella_tools::policy::ToolPolicy::allows`] enforces, so what the panel
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

/// Focus the TOOLS panel (switching to the SETTINGS tab if needed) and ask the
/// driver to re-enumerate the session's tools and re-read the settings chain.
/// The engine panel gives up the keyboard: the SETTINGS tab hosts two editors,
/// and exactly one of them is modal at a time.
pub fn focus_panel(ui: &mut DeckUi) -> DeckAction {
    ui.set_tab(DeckTab::Settings);
    ui.engine.focused = false;
    let t = &mut ui.tools;
    t.focused = true;
    t.row = 0;
    t.busy = true;
    DeckAction::Send(WorkspaceInput::ToolsRefresh)
}

// ── key handling ────────────────────────────────────────────────────────────

/// The panel's modal key map, dispatched by [`crate::deck_ui::handle_deck_key`]
/// while `ui.tools.focused`. The vocabulary is [`crate::views::engine`]'s, so
/// the two editors on one tab never need two things learned.
pub fn handle_tools_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    let plain = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META);
    match key.code {
        KeyCode::Esc => {
            ui.tools.focused = false;
            DeckAction::Handled
        }
        KeyCode::Up => {
            ui.tools.row = ui.tools.row.saturating_sub(1);
            DeckAction::Handled
        }
        KeyCode::Down => {
            let count = ui.tools.rows().len();
            if count > 0 {
                ui.tools.row = (ui.tools.row + 1).min(count - 1);
            }
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
            // row keeps reporting the key that did it, which is the honest
            // answer rather than a silent rewrite of settings the user did not
            // select.
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
/// on/off state off a narrow panel.
const NAME_W: usize = 26;

/// Render the TOOLS panel: an area-filling bordered panel (accent border while
/// it owns the keyboard, hairline otherwise), windowed rows with the selection
/// reversed, group headers, and — for anything off — the settings key and
/// scope that did it.
pub fn render_panel(ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    let t = &ui.tools;
    let (w, h) = (area.width, area.height);
    if w < 4 || h < 4 {
        return; // no readable panel fits — draw nothing rather than garbage
    }
    let inner_h = (h as usize).saturating_sub(2);
    let mut lines: Vec<Line<'static>> = Vec::new();

    let rows = t.rows();
    let (off_count, locked_count) = match &t.state {
        None => (0, 0),
        Some(state) => (
            state
                .tools
                .iter()
                .filter(|tool| !tool_enabled(state, &t.edits, tool))
                .count(),
            state.tools.iter().filter(|tool| tool.locked).count(),
        ),
    };

    match &t.state {
        None => lines.push(Line::from(Span::styled(
            format!("  {NO_SNAPSHOT_HINT}"),
            theme::muted(),
        ))),
        Some(state) => {
            let count = rows.len();
            let sel = t.row.min(count.saturating_sub(1));
            // status (1) + footer (1) bracket the rows.
            let visible = inner_h.saturating_sub(2).max(1);
            let first = scroll_window_start(count, sel, visible);
            let last = (first + visible).min(count);
            if count == 0 {
                lines.push(Line::from(Span::styled(
                    "  no tools in this session yet — r to reload",
                    theme::muted(),
                )));
            }
            for (i, row) in rows.iter().enumerate().take(last).skip(first) {
                lines.push(render_row(t, state, row, i == sel, w as usize));
            }
        }
    }

    while lines.len() < inner_h.saturating_sub(2) {
        lines.push(Line::default());
    }
    let status = t
        .status
        .clone()
        .or_else(|| t.busy.then(|| "working…".to_string()));
    lines.push(match status {
        Some(s) => Line::from(Span::styled(
            format!(" {s}"),
            Style::default().fg(theme::ACCENT),
        )),
        None => Line::default(),
    });
    lines.push(Line::from(Span::styled(
        if t.focused {
            " ⏎/space toggle · x clear · s save user · S save project · r reload · esc done"
        } else {
            " t edit tool switches"
        },
        theme::muted(),
    )));

    let mut title = format!(" tools · {off_count} off");
    if locked_count > 0 {
        title.push_str(&format!(" · {locked_count} org-locked"));
    }
    if t.dirty() {
        title.push_str(" · modified");
    }
    title.push(' ');
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if t.focused {
            theme::accent()
        } else {
            theme::rule()
        })
        .title(title);
    Paragraph::new(lines).block(block).render(area, buf);
}

/// One panel row: a group header, or `▸ name  on|off  reason`.
fn render_row(
    t: &ToolsOverlay,
    state: &ToolPolicyState,
    row: &ToolsRow,
    is_sel: bool,
    panel_w: usize,
) -> Line<'static> {
    let sel_mod = if is_sel {
        Modifier::REVERSED
    } else {
        Modifier::empty()
    };
    let marker = if is_sel { "▸ " } else { "  " };
    match row {
        ToolsRow::Group(group) => {
            let on = group_enabled(state, &t.edits, group);
            let locked = group_locked(state, group);
            let n = state
                .tools
                .iter()
                .filter(|tool| &tool.group == group)
                .count();
            let mut spans = vec![
                Span::styled(
                    marker.to_string(),
                    Style::default().fg(theme::ACCENT).add_modifier(sel_mod),
                ),
                Span::styled(
                    format!("{:<width$}", group.to_uppercase(), width = NAME_W),
                    theme::accent().add_modifier(Modifier::BOLD | sel_mod),
                ),
                Span::styled(
                    format!("{:<5}", if on { "on" } else { "off" }),
                    Style::default().fg(theme::INK).add_modifier(sel_mod),
                ),
                Span::styled(format!("{n} tools"), theme::muted().add_modifier(sel_mod)),
            ];
            if locked {
                spans.push(Span::styled(
                    " · org-locked".to_string(),
                    theme::muted().add_modifier(sel_mod),
                ));
            }
            Line::from(spans)
        }
        ToolsRow::Tool(i) => {
            let tool = &state.tools[*i];
            let on = tool_enabled(state, &t.edits, tool);
            let pending = t.edits.contains_key(&tool.name);
            let mut spans = vec![
                Span::styled(
                    marker.to_string(),
                    Style::default().fg(theme::ACCENT).add_modifier(sel_mod),
                ),
                Span::styled(
                    // Truncated one char shorter than the column so a long
                    // namespaced MCP name always keeps a gap before its state.
                    format!(
                        "  {:<width$}",
                        truncate_chars(&tool.name, NAME_W - 3),
                        width = NAME_W - 2
                    ),
                    Style::default().fg(theme::INK).add_modifier(sel_mod),
                ),
                Span::styled(
                    format!("{:<5}", if on { "on" } else { "off" }),
                    Style::default()
                        .fg(if on { theme::ACCENT } else { theme::MUTED })
                        .add_modifier(sel_mod),
                ),
            ];
            // While an edit is pending the saved reason is stale — say
            // "unsaved" instead of a sentence that describes disk.
            let tail = if pending {
                Some("unsaved".to_string())
            } else {
                off_reason(tool)
            };
            if let Some(tail) = tail {
                let room = panel_w.saturating_sub(2 + 2 + NAME_W + 5 + 1).max(8);
                spans.push(Span::styled(
                    truncate_chars(&tail, room),
                    theme::muted().add_modifier(sel_mod),
                ));
            }
            Line::from(spans)
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

    /// A session with a built-in family, an MCP server's tool, and a
    /// customer's own registered tool — the three sources the panel must show.
    fn sample_state() -> ToolPolicyState {
        ToolPolicyState {
            tools: vec![
                tool("read_file", "file"),
                tool("bash", "bash"),
                tool("start_process", "process"),
                tool("send_stdin", "process"),
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
                "[bash]".to_string(),
                "bash".to_string(),
                "[custom]".to_string(),
                // A customer's own tool gets its own section, by name.
                "deploy_to_staging".to_string(),
                "[file]".to_string(),
                "read_file".to_string(),
                "[mcp]".to_string(),
                "mcp__gh__create_issue".to_string(),
                "[process]".to_string(),
                // Sorted within the group.
                "send_stdin".to_string(),
                "start_process".to_string(),
            ],
            "groups sorted, tools sorted within them, one header each"
        );
    }

    #[test]
    fn a_row_resolves_most_specific_key_first_over_saved_switches_and_edits() {
        let mut state = sample_state();
        state.switches.insert(WILDCARD.into(), false);
        state.switches.insert("process".into(), true);
        state.switches.insert("send_stdin".into(), false);
        let none = BTreeMap::new();

        let at = |name: &str| {
            state
                .tools
                .iter()
                .find(|t| t.name == name)
                .expect("tool present")
        };
        assert!(
            !tool_enabled(&state, &none, at("read_file")),
            "wildcard off"
        );
        assert!(
            tool_enabled(&state, &none, at("start_process")),
            "group on beats the wildcard"
        );
        assert!(
            !tool_enabled(&state, &none, at("send_stdin")),
            "exact off beats the group"
        );

        // An edit at a LESS specific level must not defeat a saved exact key —
        // the panel would otherwise show a tool as on that the runtime keeps off.
        let mut edits = BTreeMap::new();
        edits.insert("process".to_string(), true);
        assert!(
            !tool_enabled(&state, &edits, at("send_stdin")),
            "a pending group grant does not outrank a saved exact denial"
        );
        // …and an edit at the SAME level does.
        edits.insert("send_stdin".to_string(), true);
        assert!(tool_enabled(&state, &edits, at("send_stdin")));
    }

    /// Deliberately over `start_process`, whose group (`process`) has a
    /// DIFFERENT name: `bash` sits in a group called `bash`, so a row toggle
    /// that wrongly wrote the group key would be indistinguishable there.
    #[test]
    fn toggling_a_tool_writes_its_exact_name_never_its_group() {
        let (model, mut ui) = open_ui();
        let rows = ui.tools.rows();
        let state = ui.tools.state.clone().unwrap();
        ui.tools.row = rows
            .iter()
            .position(
                |row| matches!(row, ToolsRow::Tool(i) if state.tools[*i].name == "start_process"),
            )
            .expect("start_process row");

        handle_deck_key(ch(' '), &model, &mut ui);
        assert_eq!(
            ui.tools.edits,
            BTreeMap::from([("start_process".to_string(), false)]),
            "the exact tool key, and only it — never the `process` group"
        );
        // Its sibling in the same group is untouched, which is the whole point
        // of writing the most specific key.
        let send_stdin = state.tools.iter().find(|t| t.name == "send_stdin").unwrap();
        assert!(tool_enabled(&state, &ui.tools.edits, send_stdin));

        // Toggling back flips the same key rather than deleting it.
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        assert_eq!(
            ui.tools.edits,
            BTreeMap::from([("start_process".to_string(), true)])
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
            .position(|row| matches!(row, ToolsRow::Group(g) if g == "process"))
            .expect("process header");
        handle_deck_key(ch(' '), &model, &mut ui);
        assert_eq!(
            ui.tools.edits,
            BTreeMap::from([("process".to_string(), false)]),
            "the group key covers the family in one line"
        );
        let state = ui.tools.state.clone().unwrap();
        for name in ["start_process", "send_stdin"] {
            let tool = state.tools.iter().find(|t| t.name == name).unwrap();
            assert!(
                !tool_enabled(&state, &ui.tools.edits, tool),
                "{name} is off"
            );
        }
        // A member edit made afterwards is more specific and wins.
        ui.tools.row = rows
            .iter()
            .position(
                |row| matches!(row, ToolsRow::Tool(i) if state.tools[*i].name == "send_stdin"),
            )
            .expect("send_stdin row");
        handle_deck_key(ch(' '), &model, &mut ui);
        assert_eq!(ui.tools.edits.get("send_stdin"), Some(&true));
    }

    #[test]
    fn a_managed_denied_row_is_locked_and_refuses_to_toggle_on() {
        let (model, mut ui) = open_ui();
        let mut state = sample_state();
        state.switches.insert("bash".into(), false);
        for tool in &mut state.tools {
            if tool.name == "bash" {
                tool.locked = true;
                tool.off = Some(ToolDenial {
                    key: "bash".into(),
                    scope: Some(ToolScope::Managed),
                });
            }
        }
        ui.tools.state = Some(state);
        ui.tools.row = 1; // the `bash` tool row

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
        let bash = state.tools.iter().find(|t| t.name == "bash").unwrap();
        assert!(!tool_enabled(state, &ui.tools.edits, bash));
        assert_eq!(
            off_reason(bash).as_deref(),
            Some("locked · \"bash\" off in org-managed settings")
        );

        // Even a forged edit (a group or wildcard grant reaching the map any
        // other way) cannot render it on.
        let mut edits = BTreeMap::new();
        edits.insert(WILDCARD.to_string(), true);
        edits.insert("bash".to_string(), true);
        assert!(
            !tool_enabled(state, &edits, bash),
            "locked short-circuits every level of the precedence ladder"
        );
        // The group header the org fully denies is locked too.
        assert!(group_locked(state, "bash"));
    }

    #[test]
    fn s_and_shift_s_send_only_the_edited_keys_at_the_chosen_scope() {
        let (model, mut ui) = open_ui();
        ui.tools.row = 1; // bash
        handle_deck_key(ch(' '), &model, &mut ui);

        handle_deck_key(ch('s'), &model, &mut ui);
        assert_eq!(
            ui.pending_inputs,
            vec![WorkspaceInput::ToolsSave {
                switches: BTreeMap::from([("bash".to_string(), false)]),
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
                switches: BTreeMap::from([("bash".to_string(), false)]),
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
        ui.tools.edits.insert("bash".into(), false);
        ui.tools.busy = true;

        // A failed save: the snapshot still says bash is on, so the edit stands.
        ingest_inbound(
            &Inbound::ToolPolicy {
                state: sample_state(),
                status: Some("save failed: cannot read".into()),
            },
            &mut model,
            &mut ui,
        );
        assert_eq!(ui.tools.edits.get("bash"), Some(&false), "still unsaved");
        assert!(!ui.tools.busy, "a snapshot always ends the in-flight op");
        assert!(ui.tools.dirty());

        // The successful echo carries the switch — the marker clears.
        let mut saved = sample_state();
        saved.switches.insert("bash".into(), false);
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
        // tools panel — one editor owns the SETTINGS keyboard at a time.
        handle_deck_key(ch('t'), &model, &mut ui);
        crate::views::engine::focus_panel(&mut ui);
        assert!(ui.engine.focused);
        assert!(
            !ui.tools.focused,
            "focusing the engine releases the tools panel"
        );
        crate::views::tools::focus_panel(&mut ui);
        assert!(ui.tools.focused);
        assert!(!ui.engine.focused, "and the other way around");
    }

    /// While one editor is modal, the other's focus key is swallowed rather
    /// than leaking into the composer behind the panel — the same rule every
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
        let mut off = tool("start_process", "process");
        off.off = Some(ToolDenial {
            key: "process".into(),
            scope: Some(ToolScope::Project),
        });
        assert_eq!(
            off_reason(&off).as_deref(),
            Some("\"process\" off in project settings")
        );
        assert_eq!(off_reason(&tool("read_file", "file")), None);
    }

    #[test]
    fn render_smoke_draws_groups_states_and_reasons() {
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

        let (_model, mut ui) = open_ui();
        let mut state = sample_state();
        state.switches.insert("process".into(), false);
        for tool in &mut state.tools {
            if tool.group == "process" {
                tool.off = Some(ToolDenial {
                    key: "process".into(),
                    scope: Some(ToolScope::User),
                });
            }
            if tool.name == "bash" {
                tool.locked = true;
                tool.off = Some(ToolDenial {
                    key: "bash".into(),
                    scope: Some(ToolScope::Managed),
                });
            }
        }
        ui.tools.state = Some(state);

        let area = Rect::new(0, 0, 96, 24);
        let mut buf = Buffer::empty(area);
        render_panel(&ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("tools ·"), "title drawn");
        assert!(text.contains("PROCESS"), "group headers drawn");
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
        assert!(text.contains("org-locked"), "the title counts locked rows");
    }
}
