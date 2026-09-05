// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The SEATS pane — which model each role of this session runs on:
//!
//! ```text
//! seats · 3
//!  default               zai/glm-5.2               from default_model
//! ▸ acme/second-opinion   anthropic/claude-opus-5   from acme
//!  vera/test_author      default                   from vera
//!  ⏎ assign · x clear · s save user · S save project · r reload · esc done
//! ```
//!
//! The third pane of the SETTINGS tab, beside AGENTS and TOOLS, and it exists
//! for the reason [`crate::views::tools`]'s module doc gives for that one:
//!
//! > MCP tools and customer-registered custom tools exist nowhere but the
//! > assembled session stack, so the rows come from the driver … never from a
//! > compiled-in table.
//!
//! Plugin seats are in exactly that position. A row is here because an
//! installed plugin declared a role, and it disappears when that plugin is
//! removed — which is the whole contract, and why this file contains no list of
//! roles and no `match` on a role name.
//!
//! # The session's own role leads the list
//!
//! [`rows`] puts the roles the driver **resolved** first
//! ([`EngineConfigState::roles`], an open list of names the driver chose —
//! today just `default`), then the roles installed plugins **declared**. The
//! driver supplies each one, so the pane still names nothing: a session with
//! no plugin shows its one role, and a plugin declaring `reviewer` makes that
//! two rows.
//!
//! Leading with the resolved role is what lets the pane answer the question a
//! reader brings to it. Plugin seats alone say which roles run on a model of
//! their own and leave "then what runs everything else?" unanswered, while
//! `default` is the model every unassigned row above already points at.
//!
//! # What a row says, and what it does not
//!
//! Each row is a name, the model it runs on, and where that answer came from.
//! For a plugin seat the name is a seat key (`<plugin-id>/<role>`,
//! `doc:roleless-core` §8.4) and the source is the plugin. For a resolved role
//! the source is the settings key that chose the model — `default_model`,
//! `agents.default.model`, `session default`, `--model (this invocation)` —
//! pre-rendered driver-side like every other cell in this module tree.
//!
//! A seat key is **rendered whole and never split**: the deck has no business
//! knowing which half is the plugin, which is why [`stella_protocol`]-side
//! callers send [`SeatRow::from`](crate::envelope::SeatRow::from) separately
//! rather than letting this pane parse it out.
//!
//! An unassigned seat renders as `default`, not as a blank. That is the truth —
//! an unassigned seat genuinely runs on the session's model — and a blank cell
//! would read as "unknown" for something the driver knows exactly.
//!
//! # An editor over the plugin rows
//!
//! `e` focuses the pane; `⏎` on a seat opens the model picker
//! ([`crate::views::engine_panel::ModelPicker`]); `x` clears one; `s`/`S`
//! save. The leading, resolved-role row is read-only — edit it on the AGENTS
//! pane. This reuses [`crate::views::engine_panel::EngineOverlay`] wholesale
//! rather than a second copy, so a seat save takes the exact path an agent
//! save does.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget, Wrap};
use stella_tui_theme::{glyph, token};

use crate::deck::DeckTab;
use crate::deck_ui::{DeckAction, DeckUi, list_nav};
use crate::envelope::{AgentScope, EngineConfigState, SeatRow, WorkspaceInput};
use crate::render::columns;
use crate::render::scroll_window_start;
use crate::views::engine_panel::{self, EngineOverlay, ModelPicker};
use crate::views::settings::SettingsPane;

/// Shown when the driver has not delivered an engine snapshot yet — a race
/// right after startup, or a driver error. The same shape (and the same
/// remedy) as the TOOLS panel's.
const NO_SNAPSHOT_HINT: &str = "waiting for the seat list — r to reload";

/// Shown when the snapshot arrived and named no rows at all — no resolved role
/// and no declared seat.
///
/// Not an apology or an error. The line says what runs instead rather than
/// implying something is missing.
const NO_SEATS_HINT: &str = "no installed plugin declares a role — every turn runs on the default \
                             model";

/// The word shown for a seat with no assignment.
///
/// The truth rather than a blank: an unassigned seat runs on the session's
/// model. A blank cell would read as "unknown" for something the driver knows
/// exactly, and would make an unassigned seat indistinguishable from one whose
/// assignment failed to resolve.
const UNASSIGNED: &str = "default";

/// Shown on `⏎`/`x` against the leading, session-resolved row: it is not a
/// `seat_models` entry, so there is nothing here for either key to write.
const NOT_A_SEAT_HINT: &str = "the session's own role isn't a seat — edit its model on the AGENTS \
                                pane";

/// Cells the seat key keeps however narrow the pane gets. Below this a key is
/// no longer identifiable, and a row whose subject cannot be read is a row
/// that says nothing.
const MIN_KEY_CELLS: usize = 12;

/// Cells between two columns.
const GAP: usize = 3;

/// The pane's rows: the roles the driver resolved, then the roles installed
/// plugins declared.
///
/// A pure fold over the snapshot, which is what lets the painter below stay a
/// painter. Every row comes from the driver — this function holds no list of
/// role names, and adding one is the defect it exists to prevent.
///
/// A resolved role takes the same row shape as a declared seat: its name, the
/// model it resolved to, and the settings key that chose that model. Its model
/// is always `Some`, because a resolved role has one by definition; an
/// unassigned plugin seat stays `None` and renders as `default`.
///
/// The editable rows start at `state.roles.len()`. Rows before that are
/// resolved roles, read-only here (see `seat_at`). Rows from it on are
/// `state.seats`, in the same order, so a combined row index always finds the
/// right seat.
#[must_use]
pub fn rows(state: &EngineConfigState) -> Vec<SeatRow> {
    state
        .roles
        .iter()
        .map(|role| SeatRow {
            key: role.role.clone(),
            model: Some(role.model.clone()),
            from: role.source.clone(),
        })
        .chain(state.seats.iter().cloned())
        .collect()
}

/// The seat at combined row `row`, or `None` when `row` is a resolved role.
/// [`rows`] puts the resolved roles first, so the seats begin at
/// `state.roles.len()`.
fn seat_at(state: &EngineConfigState, row: usize) -> Option<&SeatRow> {
    row.checked_sub(state.roles.len())
        .and_then(|i| state.seats.get(i))
}

/// The mutable counterpart of [`seat_at`], for `⏎`/`x`'s writes.
fn seat_at_mut(state: &mut EngineConfigState, row: usize) -> Option<&mut SeatRow> {
    let i = row.checked_sub(state.roles.len())?;
    state.seats.get_mut(i)
}

// ── focus / keys ────────────────────────────────────────────────────────────

/// Focus the SEATS pane's editor (`e` on the SETTINGS tab, SEATS pane). Same
/// job as [`crate::views::engine_panel::focus_panel`], for the pane that
/// shares its overlay: switch to the SETTINGS tab if needed, then ask the
/// driver to re-read the settings chain so the pane shows disk truth.
pub fn focus_panel(ui: &mut DeckUi) -> DeckAction {
    ui.set_tab(DeckTab::Settings);
    // The SETTINGS tab hosts several modal editors; exactly one owns the
    // keyboard.
    ui.tools.focused = false;
    // …and only one is on screen. Move the tab's secondary nav with the focus
    // so focusing from anywhere (a command, another pane) can never leave the
    // keyboard in an editor the user cannot see.
    ui.settings_pane = SettingsPane::Seats;
    let e = &mut ui.engine;
    e.focused = true;
    e.row = 0;
    e.edit = None;
    e.picker = None;
    // The two panes share this field. A stale AGENTS status must not read as
    // this pane's own outcome.
    e.status = None;
    e.busy = true;
    DeckAction::Send(WorkspaceInput::EngineConfigRefresh)
}

/// The SEATS pane's modal key map, dispatched by
/// [`crate::views::engine_panel::keys::handle_engine_key`] while
/// `ui.engine.focused` and `ui.settings_pane == SettingsPane::Seats`.
pub(crate) fn handle_seats_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    if ui.engine.picker.is_some() {
        return handle_picker_key(key, ui);
    }
    handle_nav_key(key, ui)
}

/// The model picker's keys — identical contract to the AGENTS pane's
/// (`crate::views::engine_panel::keys::handle_picker_key`): type to filter,
/// ↑/↓ walk the matches, ⏎ applies the picked slug to the selected seat,
/// Esc closes the picker only (the pane stays focused).
fn handle_picker_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    // Snapshot the filtered matches up front so bounds and the picked slug
    // can never disagree with what render showed.
    let matches: Vec<String> = match (&ui.engine.state, &ui.engine.picker) {
        (Some(state), Some(picker)) => engine_panel::picker_matches(state, &picker.query),
        _ => Vec::new(),
    };
    let count = matches.len();
    match key.code {
        KeyCode::Esc => {
            ui.engine.picker = None;
            DeckAction::Handled
        }
        KeyCode::Up => {
            if let Some(p) = ui.engine.picker.as_mut() {
                p.sel = p.sel.saturating_sub(1);
            }
            DeckAction::Handled
        }
        KeyCode::Down => {
            if let Some(p) = ui.engine.picker.as_mut()
                && count > 0
            {
                p.sel = (p.sel + 1).min(count - 1);
            }
            DeckAction::Handled
        }
        KeyCode::Enter => {
            let sel = ui.engine.picker.as_ref().map(|p| p.sel).unwrap_or(0);
            let picked = matches.get(sel.min(count.saturating_sub(1))).cloned();
            ui.engine.picker = None;
            let row = ui.engine.row;
            // The filter matched nothing → just close, like the AGENTS
            // picker. `open_picker` only opens over a real seat. `seat_at_mut`
            // still guards it rather than assuming — the same care
            // `agent_mut` takes on the AGENTS picker.
            if let Some(slug) = picked
                && let Some(state) = ui.engine.state.as_mut()
                && let Some(seat) = seat_at_mut(state, row)
            {
                seat.model = Some(slug);
            }
            DeckAction::Handled
        }
        KeyCode::Backspace => {
            if let Some(p) = ui.engine.picker.as_mut() {
                p.query.pop();
                p.sel = 0; // the match set changed — re-anchor
            }
            DeckAction::Handled
        }
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META) =>
        {
            if let Some(p) = ui.engine.picker.as_mut() {
                p.query.push(c);
                p.sel = 0; // the match set changed — re-anchor
            }
            DeckAction::Handled
        }
        // Modal: swallow everything else so nothing leaks behind the popup.
        _ => DeckAction::Handled,
    }
}

/// The pane's navigation/verb keys (no picker active).
fn handle_nav_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    let plain = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META);
    let count = ui.engine.state.as_ref().map(|s| rows(s).len()).unwrap_or(0);
    if list_nav::select(key, &mut ui.engine.row, count, true) {
        return DeckAction::Handled;
    }
    match key.code {
        // Return focus to the tab's left column, exactly like the AGENTS
        // pane's Esc.
        KeyCode::Esc => {
            ui.engine.focused = false;
            DeckAction::Handled
        }
        KeyCode::Enter if plain => open_picker(ui),
        KeyCode::Char('x') if plain => clear_row(ui),
        KeyCode::Char('s') if plain => engine_panel::keys::save(ui, AgentScope::User),
        KeyCode::Char('S') if plain => engine_panel::keys::save(ui, AgentScope::Project),
        KeyCode::Char('r') if plain => engine_panel::keys::refresh(ui),
        // Modal: swallow everything else.
        _ => DeckAction::Handled,
    }
}

/// `⏎` on the selected row: open the model picker over the seat, seeded on
/// its current model (the AGENTS picker's "start where you already are"). On
/// the leading, resolved-role row it does nothing but explain why.
fn open_picker(ui: &mut DeckUi) -> DeckAction {
    let Some(state) = ui.engine.state.as_ref() else {
        ui.engine.status = Some(NO_SNAPSHOT_HINT.into());
        return DeckAction::Handled;
    };
    let row = ui.engine.row;
    let Some(seat) = seat_at(state, row) else {
        ui.engine.status = Some(NOT_A_SEAT_HINT.into());
        return DeckAction::Handled;
    };
    let sel = seat
        .model
        .as_deref()
        .and_then(|current| {
            engine_panel::picker_candidates(state)
                .iter()
                .position(|c| c.as_str() == current)
        })
        .unwrap_or(0);
    ui.engine.picker = Some(ModelPicker {
        query: String::new(),
        sel,
    });
    DeckAction::Handled
}

/// `x`: clear the selected seat back to `default` (`None`). On the leading,
/// resolved-role row it does nothing but explain why.
fn clear_row(ui: &mut DeckUi) -> DeckAction {
    let row = ui.engine.row;
    let Some(state) = ui.engine.state.as_mut() else {
        ui.engine.status = Some(NO_SNAPSHOT_HINT.into());
        return DeckAction::Handled;
    };
    match seat_at_mut(state, row) {
        Some(seat) => seat.model = None,
        None => ui.engine.status = Some(NOT_A_SEAT_HINT.into()),
    }
    DeckAction::Handled
}

// ── rendering ────────────────────────────────────────────────────────────────

/// Draw the SEATS pane into `area`: header · rows · status · legend, the
/// TOOLS pane's four bands. When open, the model picker floats over all four.
/// It is the AGENTS panel's own bordered box, reused as-is.
pub fn render(ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let e = &ui.engine;
    let rows = e.state.as_ref().map(rows);

    let bands = Layout::vertical([
        Constraint::Length(1), // header · modified
        Constraint::Min(1),    // the rows, or a hint
        Constraint::Length(1), // driver status / busy
        Constraint::Length(1), // legend
    ])
    .split(area);

    render_header(e, rows.as_deref(), bands[0], buf);
    render_body(e, rows.as_deref(), bands[1], buf);
    render_status(e, bands[2], buf);
    render_legend(e.focused, bands[3], buf);

    if e.picker.is_some() {
        render_picker(e, area, buf);
    }
}

/// ` seats · N`, plus `modified` on the right edge once the working copy
/// differs from the driver's last snapshot. The AGENTS strip's own marker,
/// off the same shared `dirty()`.
fn render_header(e: &EngineOverlay, rows: Option<&[SeatRow]>, area: Rect, buf: &mut Buffer) {
    let muted = Style::new().fg(token::MUTED);
    let mut spans = vec![Span::styled(" seats", muted)];
    if let Some(rows) = rows.filter(|rows| !rows.is_empty()) {
        spans.push(Span::styled(
            format!(" · {}", rows.len()),
            Style::new().fg(token::DIM),
        ));
    }
    if e.dirty() {
        let used: usize = spans.iter().map(Span::width).sum();
        let marker = "modified ";
        let width = area.width as usize;
        if used + marker.len() < width {
            spans.push(Span::raw(" ".repeat(width - used - marker.len())));
            spans.push(Span::styled(marker, Style::new().fg(token::GOLD_BRIGHT)));
        }
    }
    Paragraph::new(Line::from(spans)).render(area, buf);
}

/// The windowed rows, or the wait/empty hint. While focused it scrolls to
/// keep the selection visible ([`scroll_window_start`], the same windowing
/// AGENTS and TOOLS use). At rest, row 0, the window starts at the top like
/// a plain list.
fn render_body(e: &EngineOverlay, rows: Option<&[SeatRow]>, area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);

    let rows = match rows {
        None => {
            hint(NO_SNAPSHOT_HINT, muted, area, buf);
            return;
        }
        Some([]) => {
            hint(NO_SEATS_HINT, muted, area, buf);
            return;
        }
        Some(rows) => rows,
    };

    // One row is spent on the `⋯ n more` tail when the list overruns the
    // pane, because a list that simply stops at the last drawn row claims to
    // be complete.
    let height = area.height as usize;
    let overflow = rows.len() > height;
    let visible = if overflow {
        height.saturating_sub(1).max(1)
    } else {
        height
    };
    let sel = e.row.min(rows.len().saturating_sub(1));
    let first = scroll_window_start(rows.len(), sel, visible);
    let last = (first + visible).min(rows.len());
    let (key_w, model_w) = column_widths(&rows[first..last], area.width as usize);

    let mut lines: Vec<Line<'static>> = (first..last)
        .map(|i| {
            seat_line(
                &rows[i],
                e.focused && i == sel,
                key_w,
                model_w,
                text,
                muted,
                dim,
            )
        })
        .collect();
    if overflow {
        lines.push(Line::from(Span::styled(
            format!(" ⋯ {} more", rows.len() - visible),
            dim,
        )));
    }
    Paragraph::new(lines).render(area, buf);
}

/// One row: the selection marker, the seat key, the model (dimmed when it is
/// the unassigned default), and where it came from.
fn seat_line(
    row: &SeatRow,
    is_sel: bool,
    key_w: usize,
    model_w: usize,
    text: Style,
    muted: Style,
    dim: Style,
) -> Line<'static> {
    let assigned = row.model.is_some();
    let model = row.model.as_deref().unwrap_or(UNASSIGNED);
    let spans = vec![
        Span::styled(
            if is_sel {
                format!("{} ", glyph::COLLAPSED)
            } else {
                "  ".to_string()
            },
            Style::new().fg(token::GOLD),
        ),
        Span::styled(pad(&fit(&row.key, key_w), key_w), text),
        Span::raw(" ".repeat(GAP)),
        // An assignment is a decision someone made and is worth reading; the
        // inherited default is context. Same reasoning as the TOOLS pane
        // giving an explicit switch more weight than an inherited one.
        Span::styled(
            pad(&fit(model, model_w), model_w),
            if assigned { text } else { muted },
        ),
        Span::raw(" ".repeat(GAP)),
        Span::styled(format!("from {}", row.from), dim),
    ];
    let mut line = Line::from(spans);
    if is_sel {
        line.style = Style::new().bg(token::HL).add_modifier(Modifier::BOLD);
    }
    line
}

/// The status line, or the busy hint. Same field AGENTS draws
/// ([`crate::views::engine_panel::paint`]); this pane shares the copy.
fn render_status(e: &EngineOverlay, area: Rect, buf: &mut Buffer) {
    let status = e
        .status
        .clone()
        .or_else(|| e.busy.then(|| "working…".to_string()));
    let Some(status) = status else { return };
    Paragraph::new(Line::from(Span::styled(
        format!(" {status}"),
        Style::new().fg(token::GOLD),
    )))
    .render(area, buf);
}

/// The legend tracks focus: while the pane owns the keyboard it teaches its
/// verbs; otherwise it teaches the one key that grants focus.
fn render_legend(focused: bool, area: Rect, buf: &mut Buffer) {
    let key = Style::new().fg(token::MUTED);
    let dim = Style::new().fg(token::DIM);
    let pairs: &[(&str, &str)] = if focused {
        &[
            ("⏎", "assign"),
            ("x", "clear"),
            ("s", "save user"),
            ("S", "save project"),
            ("r", "reload"),
            ("esc", "done"),
        ]
    } else {
        &[("e", "edit seats")]
    };
    let mut spans = vec![Span::raw(" ")];
    for (i, (chord, does)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(Span::styled(*chord, key));
        spans.push(Span::styled(format!(" {does}"), dim));
    }
    Paragraph::new(Line::from(spans)).render(area, buf);
}

/// The model-picker sub-overlay, centered over the pane. The AGENTS panel's
/// own popup (`crate::views::engine_panel::paint::render_model_picker`), keyed
/// here on the selected seat instead of the selected agent role.
fn render_picker(e: &EngineOverlay, area: Rect, buf: &mut Buffer) {
    let Some(picker) = &e.picker else { return };
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let w = area.width.min(56);
    let h = area.height.min(16);
    if w < 4 || h < 4 {
        return;
    }
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    Clear.render(popup, buf);

    let state = e.state.as_ref();
    let seat = state.and_then(|s| seat_at(s, e.row));
    let seat_label = seat.map(|s| s.key.as_str()).unwrap_or("seat");
    let current = seat.and_then(|s| s.model.clone());
    let matches = state
        .map(|s| engine_panel::picker_matches(s, &picker.query))
        .unwrap_or_default();

    let inner_h = (h as usize).saturating_sub(2);
    let visible = inner_h.saturating_sub(2).max(1);
    let sel = picker.sel.min(matches.len().saturating_sub(1));
    let first = scroll_window_start(matches.len(), sel, visible);
    let last = (first + visible).min(matches.len());

    let mut lines: Vec<Line<'static>> = vec![Line::from(vec![
        Span::styled("filter ", muted),
        Span::styled(picker.query.clone(), Style::new().fg(token::TEXT)),
        Span::styled("▏", Style::new().fg(token::GOLD)),
    ])];

    if state.is_none() {
        lines.push(Line::from(Span::styled(
            format!("  {NO_SNAPSHOT_HINT}"),
            muted,
        )));
    } else if matches.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no models match — Backspace to widen",
            muted,
        )));
    }
    for (i, slug) in matches.iter().enumerate().take(last).skip(first) {
        let is_sel = i == sel;
        let mut spans = vec![
            Span::styled(
                if is_sel {
                    format!("{} ", glyph::COLLAPSED)
                } else {
                    "  ".to_string()
                },
                Style::new().fg(token::GOLD),
            ),
            Span::styled(
                columns::head(slug, (w as usize).saturating_sub(6)),
                Style::new().fg(token::TEXT),
            ),
        ];
        if current.as_deref() == Some(slug.as_str()) {
            spans.push(Span::styled("  · current", dim));
        }
        let mut line = Line::from(spans);
        if is_sel {
            line.style = Style::new().bg(token::HL).add_modifier(Modifier::BOLD);
        }
        lines.push(line);
    }

    // Pad so the legend sits on the last interior row regardless of matches.
    while lines.len() < inner_h.saturating_sub(1).max(1) {
        lines.push(Line::default());
    }
    lines.push(Line::from(vec![
        Span::styled(" type", muted),
        Span::styled(" to filter · ", dim),
        Span::styled("↑↓", muted),
        Span::styled(" select · ", dim),
        Span::styled("⏎", muted),
        Span::styled(" pick · ", dim),
        Span::styled("esc", muted),
        Span::styled(" back", dim),
    ]));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(token::BORDER))
        .title(Line::from(vec![
            Span::styled(" model ", Style::new().fg(token::GOLD)),
            Span::styled(format!("· {seat_label} · "), dim),
            Span::styled(format!("{} available ", matches.len()), muted),
        ]));
    Paragraph::new(lines).block(block).render(popup, buf);
}

// ── pure column layout ───────────────────────────────────────────────────────

/// The two padded columns' widths for `rows` in a pane `width` cells wide.
///
/// Shrinking order is meaning order. The plugin name goes first, because it is
/// already the head of every key beside it; the model next; the seat key last,
/// down to [`MIN_KEY_CELLS`] and no further.
fn column_widths(rows: &[SeatRow], width: usize) -> (usize, usize) {
    // A seat key, a model slug, and a plugin name are all text a plugin
    // manifest chose (`doc:roleless-core` §8.4). This pane has no say in
    // it. So every width here is a display column, never a `char`.
    let cells = columns::width;
    let mut key_w = rows.iter().map(|r| cells(&r.key)).max().unwrap_or(0);
    let mut model_w = rows
        .iter()
        .map(|r| cells(r.model.as_deref().unwrap_or(UNASSIGNED)))
        .max()
        .unwrap_or(0);
    let from_w = rows
        .iter()
        .map(|r| cells(&r.from) + "from ".len())
        .max()
        .unwrap_or(0);

    // The two-cell selection marker, then the three columns with a gap
    // between each.
    let budget = width.saturating_sub(2 + GAP * 2);
    let mut over = (key_w + model_w + from_w).saturating_sub(budget);
    if over > 0 {
        // The `from` column is the one that gives way first, and it gives way
        // by being pushed off the right edge rather than by being padded
        // shorter: it is last on the row, so the paragraph clips it.
        over = over.saturating_sub(from_w);
    }
    let shed = over.min(model_w);
    model_w -= shed;
    over -= shed;
    key_w = key_w.saturating_sub(over).max(MIN_KEY_CELLS.min(key_w));
    (key_w, model_w)
}

/// `s` cut to `width` display columns, with an ellipsis where it was cut.
fn fit(s: &str, width: usize) -> String {
    columns::head(s, width)
}

/// `s` padded to `width` display columns. `format!("{s:width$}")` counts
/// `char`s, which under-pads a key holding a CJK or emoji glyph.
fn pad(s: &str, width: usize) -> String {
    columns::pad(s, width)
}

/// One muted line of explanation where the rows would have been.
fn hint(message: &str, style: Style, area: Rect, buf: &mut Buffer) {
    Paragraph::new(Line::from(Span::styled(format!(" {message}"), style)))
        .wrap(Wrap { trim: false })
        .render(area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::WorkspaceModel;

    fn ui_with(state: Option<EngineConfigState>) -> DeckUi {
        let mut ui = DeckUi::default();
        ui.splash.skip();
        ui.set_tab(DeckTab::Settings);
        // `pristine` starts equal to `state`, so `dirty()` reads false until
        // a test actually edits something — the same discipline
        // `engine_panel::fixtures::open_ui` follows.
        ui.engine.pristine = state.clone();
        ui.engine.state = state;
        ui
    }

    fn draw(ui: &DeckUi, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        render(ui, area, &mut buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn seat(key: &str, model: Option<&str>, from: &str) -> SeatRow {
        SeatRow {
            key: key.to_string(),
            model: model.map(str::to_string),
            from: from.to_string(),
        }
    }

    /// One resolved role as the driver sends it: a name, the model it landed
    /// on, and the settings key that chose it.
    fn resolved(role: &str, model: &str, source: &str) -> crate::envelope::RoleWiringRow {
        crate::envelope::RoleWiringRow {
            role: role.to_string(),
            model: model.to_string(),
            source: source.to_string(),
            ..Default::default()
        }
    }

    fn snapshot(
        roles: Vec<crate::envelope::RoleWiringRow>,
        seats: Vec<SeatRow>,
    ) -> EngineConfigState {
        EngineConfigState {
            roles,
            seats,
            ..Default::default()
        }
    }

    /// The keys of the rows [`rows`] folds out of a snapshot, in order.
    fn keys(state: &EngineConfigState) -> Vec<String> {
        rows(state).into_iter().map(|row| row.key).collect()
    }

    // ── the fold ─────────────────────────────────────────────────────────

    /// **The witness.** One plugin declaring `reviewer`, and the pane is two
    /// rows: the session's own role first, then the plugin's.
    #[test]
    fn the_session_role_leads_the_plugin_seats() {
        let state = snapshot(
            vec![resolved("default", "zai/glm-5.2", "default_model")],
            vec![seat("acme/reviewer", None, "acme")],
        );
        assert_eq!(keys(&state), ["default", "acme/reviewer"]);

        let text = draw(&ui_with(Some(state)), 90, 12);
        assert!(text.contains("seats · 2"), "{text}");
        assert!(text.contains("zai/glm-5.2"), "{text}");
        assert!(text.contains("from default_model"), "{text}");
        assert!(text.contains("acme/reviewer"), "{text}");
    }

    /// A fresh install has no plugin, and the pane is the one row the session
    /// does have rather than a hint that it has none.
    #[test]
    fn a_session_with_no_plugin_still_shows_its_own_role() {
        let state = snapshot(
            vec![resolved(
                "default",
                "anthropic/claude-opus-5",
                "session default",
            )],
            Vec::new(),
        );
        assert_eq!(keys(&state), ["default"]);

        let text = draw(&ui_with(Some(state)), 90, 12);
        assert!(text.contains("anthropic/claude-opus-5"), "{text}");
        assert!(!text.contains("no installed plugin"), "{text}");
    }

    /// The fold names nothing. A driver that calls the session's own role
    /// something other than `default` gets that word back, which is what
    /// proves the row is read rather than written here — a fold that
    /// hardcoded `"default"` would pass every other test above and still
    /// fail this one.
    #[test]
    fn the_leading_row_is_named_by_the_driver_not_by_this_pane() {
        let state = snapshot(
            vec![resolved("lead", "zai/glm-5.2", "default_model")],
            vec![],
        );
        assert_eq!(keys(&state), ["lead"]);
    }

    /// A snapshot the driver sent no rows in draws the hint, not an invented
    /// `default` row: the pane must not answer a question the driver has not.
    #[test]
    fn an_empty_snapshot_invents_no_row() {
        assert!(rows(&EngineConfigState::default()).is_empty());
    }

    /// An unassigned plugin seat stays unassigned through the fold. Filling it
    /// with the leading row's model would make it indistinguishable from a
    /// seat somebody pinned there.
    #[test]
    fn the_fold_leaves_an_unassigned_seat_unassigned() {
        let state = snapshot(
            vec![resolved("default", "zai/glm-5.2", "default_model")],
            vec![seat("vera/test_author", None, "vera")],
        );
        let folded = rows(&state);
        assert_eq!(folded[0].model.as_deref(), Some("zai/glm-5.2"));
        assert_eq!(folded[1].model, None);
    }

    // ── rendering (unfocused / browse) ──────────────────────────────────

    /// **The witness.** A role no core enum has ever heard of renders, because
    /// the rows come from the driver rather than from a compiled-in table.
    #[test]
    fn a_role_core_has_never_heard_of_renders() {
        let state = snapshot(
            Vec::new(),
            vec![seat(
                "acme/second-opinion",
                Some("anthropic/claude-opus-5"),
                "acme",
            )],
        );
        let text = draw(&ui_with(Some(state)), 90, 12);
        assert!(text.contains("acme/second-opinion"), "{text}");
        assert!(text.contains("anthropic/claude-opus-5"), "{text}");
        assert!(text.contains("from acme"), "{text}");
    }

    /// An unassigned seat says `default`, because that is what it runs on.
    #[test]
    fn an_unassigned_seat_names_the_default_rather_than_blanking() {
        let state = snapshot(
            Vec::new(),
            vec![seat("stella-plan/planner", None, "stella-plan")],
        );
        let text = draw(&ui_with(Some(state)), 90, 12);
        assert!(text.contains("stella-plan/planner"), "{text}");
        assert!(text.contains(UNASSIGNED), "{text}");
    }

    /// No plugins is the ordinary fresh-install state, and the pane says what
    /// happens instead rather than apologising or showing an empty box.
    #[test]
    fn no_seats_explains_the_default_rather_than_erroring() {
        let text = draw(&ui_with(Some(EngineConfigState::default())), 90, 12);
        assert!(
            text.contains("no installed plugin declares a role"),
            "{text}"
        );
        assert!(text.contains("default model"), "{text}");
    }

    /// No snapshot is a different state from no seats, and must not be
    /// rendered as "you have no plugins" — that would be the deck answering a
    /// question the driver has not answered yet.
    #[test]
    fn a_missing_snapshot_is_not_reported_as_an_empty_seat_list() {
        let text = draw(&ui_with(None), 90, 12);
        assert!(text.contains("waiting for the seat list"), "{text}");
        assert!(!text.contains("no installed plugin"), "{text}");
    }

    /// The key is rendered whole. Splitting it to show the plugin separately
    /// would be the deck reading a string it is contractually ignorant of.
    #[test]
    fn the_seat_key_is_rendered_whole() {
        let state = snapshot(
            Vec::new(),
            vec![seat(
                "vera/test_author",
                Some("openrouter/openai/gpt-5.5"),
                "vera",
            )],
        );
        let text = draw(&ui_with(Some(state)), 90, 12);
        assert!(text.contains("vera/test_author"), "{text}");
    }

    /// The pane draws no border of its own — the picker is the one bordered
    /// box, and only while it is open.
    #[test]
    fn the_pane_draws_no_border() {
        let state = snapshot(
            Vec::new(),
            vec![seat("acme/reviewer", Some("zai/glm-5"), "acme")],
        );
        let text = draw(&ui_with(Some(state)), 60, 6);
        assert!(
            !text.contains('│') && !text.contains('╭') && !text.contains('┌'),
            "{text}"
        );
        assert!(text.starts_with(" seats · 1"), "{text}");
    }

    /// A list longer than the pane names what it could not draw. Stopping at
    /// the last row that fit would claim the list ended there.
    #[test]
    fn an_overrunning_list_counts_what_it_could_not_draw() {
        let seats: Vec<SeatRow> = (0..9)
            .map(|i| seat(&format!("acme/role-{i}"), None, "acme"))
            .collect();
        let state = snapshot(Vec::new(), seats);
        // header(1) + status(1) + legend(1) leave 6 rows for the list; 9 rows
        // over 6 draws 5 (one spent on the tail) and names 4 more.
        let text = draw(&ui_with(Some(state)), 60, 9);
        assert!(text.contains("acme/role-0"), "{text}");
        assert!(text.contains("acme/role-4"), "{text}");
        assert!(text.contains("⋯ 4 more"), "{text}");
        assert!(!text.contains("acme/role-8"), "{text}");
    }

    /// A pane too narrow for all three columns keeps the seat key readable and
    /// lets the plugin name fall off the right edge.
    #[test]
    fn a_narrow_pane_keeps_the_key_and_sheds_the_plugin_name() {
        let state = snapshot(
            Vec::new(),
            vec![seat(
                "stella-plan/planner",
                Some("anthropic/claude-opus-5"),
                "stella-plan",
            )],
        );
        let text = draw(&ui_with(Some(state)), 34, 4);
        let row = text.lines().nth(1).unwrap_or_default().to_string();
        assert!(row.contains("stella-plan/planner"), "{row}");
        assert!(!row.contains("from stella-plan"), "{row}");
    }

    /// A CJK seat key is measured in the columns it draws, not its chars.
    #[test]
    fn columns_measures_a_wide_character_key_in_display_columns() {
        let wide_key = "圈".repeat(13);
        let rows = [
            seat("acme/reviewer", Some("m"), "acme"),
            seat(&wide_key, Some("m"), "acme2"),
        ];
        assert_eq!(column_widths(&rows, 90), (26, 1));
    }

    /// The same rows, rendered: the CJK row's `from` column lands where the
    /// ASCII row's does, because the key column was sized to the widest key's
    /// real columns, not its char count.
    #[test]
    fn a_wide_character_key_does_not_shift_a_shared_column() {
        let wide_key = "圈".repeat(13);
        let state = snapshot(
            Vec::new(),
            vec![
                seat("acme/reviewer", Some("m"), "acme"),
                seat(&wide_key, Some("m"), "acme2"),
            ],
        );
        let (key_w, model_w) = column_widths(&rows(&state), 90);
        let area = Rect::new(0, 0, 90, 12);
        let mut buf = Buffer::empty(area);
        render(&ui_with(Some(state)), area, &mut buf);
        let from_col = 2 + key_w + GAP + model_w + GAP;
        let ascii_row_y = 1; // row 0 is the head strip
        let wide_row_y = 2;
        assert_eq!(
            buf.cell((from_col as u16, ascii_row_y)).map(|c| c.symbol()),
            Some("f"),
            "the ascii row's `from` should start at the shared column"
        );
        assert_eq!(
            buf.cell((from_col as u16, wide_row_y)).map(|c| c.symbol()),
            Some("f"),
            "the CJK row's `from` shifted off the shared column"
        );
    }

    // ── the editor ───────────────────────────────────────────────────────

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    fn declared_state() -> EngineConfigState {
        let mut state = snapshot(
            vec![resolved("default", "zai/glm-5.2", "default_model")],
            vec![
                seat("acme/reviewer", None, "acme"),
                seat("vera/verifier", Some("zai/glm-5.2"), "vera"),
            ],
        );
        state.allowed_models = vec!["anthropic/claude-opus-5".into(), "zai/glm-5.2".into()];
        state
    }

    /// `e` on the SEATS pane focuses the shared overlay and moves the row
    /// selection onto the first plugin seat's model — the round trip the
    /// issue asks for, driven entirely through
    /// [`crate::deck_ui::handle_deck_key`] the way a real keypress would.
    #[test]
    fn e_enter_assigns_a_seat_and_s_saves_it() {
        let mut ui = ui_with(Some(declared_state()));
        ui.settings_pane = SettingsPane::Seats;

        let model = WorkspaceModel::new();
        let action = crate::deck_ui::handle_deck_key(ch('e'), &model, &mut ui);
        assert_eq!(
            action,
            DeckAction::Send(WorkspaceInput::EngineConfigRefresh)
        );
        assert!(ui.engine.focused, "e focused the shared overlay");
        assert_eq!(ui.engine.row, 0);

        // Row 0 is the resolved `default` role: not a seat, and both verbs
        // say so rather than doing nothing silently.
        crate::deck_ui::handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        assert!(ui.engine.picker.is_none(), "the leading row has no picker");
        assert_eq!(ui.engine.status.as_deref(), Some(NOT_A_SEAT_HINT));

        // Down twice lands on `acme/reviewer`, the first assignable seat.
        crate::deck_ui::handle_deck_key(key(KeyCode::Down), &model, &mut ui);
        assert_eq!(ui.engine.row, 1);

        crate::deck_ui::handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        assert!(ui.engine.picker.is_some(), "⏎ on a seat opens the picker");
        for c in "opus".chars() {
            crate::deck_ui::handle_deck_key(ch(c), &model, &mut ui);
        }
        crate::deck_ui::handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        assert_eq!(ui.engine.picker, None, "⏎ closes the picker");
        let state = ui.engine.state.as_ref().unwrap();
        assert_eq!(
            state.seats[0].model.as_deref(),
            Some("anthropic/claude-opus-5"),
            "the pick landed on the seat, not the resolved role"
        );
        assert!(ui.engine.dirty(), "the pick is a local edit until saved");

        let action = crate::deck_ui::handle_deck_key(ch('s'), &model, &mut ui);
        assert_eq!(action, DeckAction::Handled, "the save rides pending_inputs");
        assert_eq!(
            ui.pending_inputs,
            vec![WorkspaceInput::EngineConfigSave {
                state: ui.engine.state.clone().unwrap(),
                scope: AgentScope::User,
            }],
            "s sends the whole working copy — the same path the AGENTS pane's s takes"
        );
    }

    /// `x` clears an assigned seat back to `default` rather than writing
    /// whatever the default model happens to be today.
    #[test]
    fn x_clears_a_seat_to_default() {
        let mut ui = ui_with(Some(declared_state()));
        ui.settings_pane = SettingsPane::Seats;
        ui.engine.focused = true;
        ui.engine.row = 2; // vera/verifier, pinned to zai/glm-5.2

        crate::deck_ui::handle_deck_key(ch('x'), &crate::deck::WorkspaceModel::new(), &mut ui);
        assert_eq!(ui.engine.state.as_ref().unwrap().seats[1].model, None);
    }

    /// Esc on the pane's own nav hands the keyboard back to the tab, exactly
    /// like the AGENTS pane's.
    #[test]
    fn esc_returns_focus_to_the_tab() {
        let mut ui = ui_with(Some(declared_state()));
        ui.settings_pane = SettingsPane::Seats;
        ui.engine.focused = true;

        crate::deck_ui::handle_deck_key(
            key(KeyCode::Esc),
            &crate::deck::WorkspaceModel::new(),
            &mut ui,
        );
        assert!(!ui.engine.focused);
    }
}
