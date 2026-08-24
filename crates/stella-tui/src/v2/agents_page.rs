// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The AGENTS page (`←` twice from an empty prompt on the SESSION tab): a
//! **full-frame** surface — not a centered dialog — for running the fleet.
//!
//! ```text
//! AGENTS · 1 waiting · 2 working · 3 completed                r refresh
//!
//! WORKING
//! ▸ ◆ req:1  running · 0:24 · zai/glm-5.2 · $0.03
//!     Fix the parser panic on empty input.
//!   ◆ sub:3  running · 5:02 · zai/glm-5.2 · $0.12
//!     Simplify the crate READMEs.
//!
//! COMPLETED
//!   ● stella: wire the dedup digest              this session · 3h ago
//!   ○ stella: fix the parser                          ↩ resume · 2d ago
//!
//! ❯ describe a task for a new session
//!   ⏎ start a new agent · ↑↓ select · ⏎ open · n new session · esc back
//! ```
//!
//! The page's own composer is the point: describe a task, `⏎`, and a fresh
//! sub-agent lane starts on it ([`WorkspaceInput::SpawnLane`]) — whatever the
//! lead is doing. `n` starts a brand-new **full** session instead
//! ([`WorkspaceInput::SessionNew`]): the driver parks this one and opens an
//! empty record, the SESSIONS overlay's own verb. The rows are the same two populations the SUB-AGENTS and
//! SESSIONS overlays show one each of: this session's lanes (live first) and
//! the machine's session registry ([`crate::deck_ui::sessions`]'s row rules).
//!
//! **The command menu here is scoped.** The page offers only the commands
//! that make sense over a fleet view ([`PAGE_COMMANDS`]) — `/model` with its
//! argument menu, `/info`, `/theme`, `/help` — and each leaves as
//! [`WorkspaceInput::Command`]. Anything else typed (`/export` is the named
//! case) is refused with a footer notice rather than silently acting on the
//! session behind the page.
//!
//! Most of that set is queue-free and answers at once. `/model <spec>` is
//! the exception and is sent all the same: the driver declines it on the
//! queue-free path (since #4617 it switches the running session's model,
//! which only the driver loop can do) and then applies it between turns, or
//! backlogs it when a turn is in flight. So the page offers one route and
//! the driver decides what each command costs.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget};
use stella_tui_theme::token;

use crate::composer::{
    Composer, EnterAction, PaletteState, SlashCommand, SlashPopupOutcome, args, classify_enter,
    handle_edit_key, handle_slash_popup_key,
};
use crate::deck::{AgentEntry, DeckTab, WorkspaceModel};
use crate::deck_ui::sessions::{is_live, visible_session_rows};
use crate::deck_ui::{DeckAction, DeckUi};
use crate::envelope::{AgentControl, AgentStatus, SessionInfo, WorkspaceInput};
use crate::views::cards;

/// The second `←` must land inside this window to open the page.
pub const LEFT_DOUBLE_WINDOW: Duration = Duration::from_millis(1500);

/// The commands the page's menu offers — the fleet-appropriate, queue-free
/// subset of the deck vocabulary. Everything else is refused with a notice:
/// most deck commands act on the session view *behind* this page (`/files`,
/// `/inspect`), and `/export` is the named example of one that must not run
/// from here at all.
pub const PAGE_COMMANDS: &[&str] = &["/model", "/info", "/models", "/theme", "/help"];

/// The page's state: whether it is up, the selected row, its own composer
/// (never the deck's — opening the page must not disturb a half-typed
/// prompt), and the `←` chord latch.
#[derive(Debug, Clone, Default)]
pub struct AgentsPage {
    pub open: bool,
    pub sel: usize,
    /// The page's own prompt — "describe a task for a new session".
    pub composer: Composer,
    /// Selected row in the page's slash / argument popup.
    pub menu_selected: usize,
    /// `⌃x` was pressed once on the selected lane; the next kills, any other
    /// key disarms — the SUB-AGENTS overlay's own two-press rule.
    pub kill_armed: bool,
    /// One footer line after a verb (a refused command, a dispatched task).
    /// Cleared by the next key.
    pub notice: Option<String>,
    /// When the first empty-prompt `←` on the SESSION tab armed the chord.
    /// Consumed by every key ([`crate::deck_ui::handle_deck_key`]'s take), so
    /// only two *consecutive* presses open the page.
    pub left_armed_at: Option<Instant>,
}

/// One page row: a lane of this session, or a registry session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// Index into `model.agents`.
    Lane(usize),
    /// Index into `ui.sessions`.
    Session(usize),
}

/// The rows in paint order: working (live lanes, then live sessions), then
/// completed (finished lanes, then the registry's recent history). The
/// session rows follow the SESSIONS overlay's visibility rules, minus this
/// deck's own row — the lead is already on the page as its lanes, and a row
/// that resumes into itself is a trap.
pub fn rows(model: &WorkspaceModel, ui: &DeckUi) -> Vec<Row> {
    let session_idx = |s: &SessionInfo| ui.sessions.iter().position(|x| x.id == s.id);
    let sessions = visible_session_rows(ui, model.now_ms);
    let lanes = crate::v2::subagents::lanes(model);
    let mut out = Vec::new();
    for (i, lane) in &lanes {
        if !lane.status.is_terminal() {
            out.push(Row::Lane(*i));
        }
    }
    for s in &sessions {
        if !s.mine && is_live(s.phase) {
            out.extend(session_idx(s).map(Row::Session));
        }
    }
    for (i, lane) in &lanes {
        if lane.status.is_terminal() {
            out.push(Row::Lane(*i));
        }
    }
    for s in &sessions {
        if !s.mine && !is_live(s.phase) {
            out.extend(session_idx(s).map(Row::Session));
        }
    }
    out
}

/// The header's three counts over [`rows`]: waiting (a paused/gated lane, a
/// needs-input session), working, completed.
pub fn counts(model: &WorkspaceModel, ui: &DeckUi) -> (usize, usize, usize) {
    let (mut waiting, mut working, mut completed) = (0, 0, 0);
    for row in rows(model, ui) {
        match row {
            Row::Lane(i) => match model.agents.get(i).map(|a| a.status) {
                Some(AgentStatus::Paused) | Some(AgentStatus::WaitingInput) => waiting += 1,
                Some(s) if s.is_terminal() => completed += 1,
                Some(_) => working += 1,
                None => {}
            },
            Row::Session(i) => match ui.sessions.get(i).map(|s| s.phase) {
                Some(crate::envelope::SessionPhase::NeedsInput) => waiting += 1,
                Some(p) if is_live(p) => working += 1,
                Some(_) => completed += 1,
                None => {}
            },
        }
    }
    (waiting, working, completed)
}

/// The double-`←` opener. `armed` is the latch [`crate::deck_ui`] takes at
/// the top of every key, so any key between the two presses disarms. Claims
/// the key only on the SESSION tab from an empty composer with no modifiers
/// — everywhere else `←` keeps meaning what it meant. The first press arms
/// and is consumed (it no longer wraps backward to the last tab; `→`, Tab
/// and Shift-Tab all still cycle), the second opens the page.
pub fn left_left(key: KeyEvent, armed: Option<Instant>, ui: &mut DeckUi) -> Option<DeckAction> {
    if ui.tab != DeckTab::Session
        || !ui.composer.is_empty()
        || !key.modifiers.is_empty()
        || !matches!(key.code, KeyCode::Left)
    {
        return None;
    }
    if armed.is_some_and(|at| at.elapsed() <= LEFT_DOUBLE_WINDOW) {
        return Some(open(ui));
    }
    ui.agents_page.left_armed_at = Some(Instant::now());
    Some(DeckAction::Handled)
}

/// Open the page and ask the driver for a fresh session-registry snapshot.
pub fn open(ui: &mut DeckUi) -> DeckAction {
    ui.agents_page.open = true;
    ui.agents_page.sel = 0;
    ui.agents_page.menu_selected = 0;
    ui.agents_page.kill_armed = false;
    ui.agents_page.notice = None;
    ui.agents_page.left_armed_at = None;
    DeckAction::Send(WorkspaceInput::SessionsRefresh)
}

/// The page's slash vocabulary: the deck's, narrowed to [`PAGE_COMMANDS`].
fn page_commands(ui: &DeckUi) -> Vec<SlashCommand> {
    ui.slash_commands
        .iter()
        .filter(|c| PAGE_COMMANDS.contains(&c.name.as_str()))
        .cloned()
        .collect()
}

/// The page's slash-popup matches (its scoped vocabulary), or empty.
fn slash_matches(ui: &DeckUi) -> Vec<String> {
    crate::composer::slash_popup_matches(
        &ui.agents_page.composer,
        &page_commands(ui),
        &PaletteState::default(),
    )
}

/// The page's `/model` argument-menu matches, or empty.
fn model_arg_matches(model: &WorkspaceModel, ui: &DeckUi) -> Vec<String> {
    args::arg_matches(
        &ui.agents_page.composer,
        "/model",
        &crate::views::picker::typeahead_candidates(model, ui),
    )
}

/// The page's keys. Modal and full-frame: every key belongs to the page
/// until Esc (or `q` from an empty composer) closes it.
pub fn handle_key(key: KeyEvent, model: &WorkspaceModel, ui: &mut DeckUi) -> DeckAction {
    let list = rows(model, ui);
    let count = list.len();
    ui.agents_page.sel = ui.agents_page.sel.min(count.saturating_sub(1));
    ui.agents_page.notice = None;
    let armed = std::mem::take(&mut ui.agents_page.kill_armed);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let composer_empty = ui.agents_page.composer.is_empty();

    // The argument menu first (`/model <fragment>`), then the scoped slash
    // menu — the same precedence the deck composer applies.
    let arg = model_arg_matches(model, ui);
    if !arg.is_empty()
        && let Some(outcome) = args::handle_arg_popup_key(
            key,
            "/model",
            &arg,
            &mut ui.agents_page.composer,
            &mut ui.agents_page.menu_selected,
        )
    {
        return match outcome {
            SlashPopupOutcome::Handled => DeckAction::Handled,
            SlashPopupOutcome::Submit(text) => submit(ui, text),
        };
    }
    let slash = slash_matches(ui);
    if !slash.is_empty()
        && let Some(outcome) = handle_slash_popup_key(
            key,
            &slash,
            &mut ui.agents_page.composer,
            &mut ui.agents_page.menu_selected,
        )
    {
        return match outcome {
            SlashPopupOutcome::Handled => DeckAction::Handled,
            SlashPopupOutcome::Submit(text) => submit(ui, text),
        };
    }

    // Submitting and editing the page composer.
    if !ui.agents_page.composer.is_blank() {
        match classify_enter(&key) {
            EnterAction::Submit => {
                return match ui.agents_page.composer.take_submission() {
                    Some(sub) => submit(ui, sub.text),
                    None => DeckAction::Handled,
                };
            }
            EnterAction::Newline => {
                ui.agents_page.composer.insert_newline();
                return DeckAction::Handled;
            }
            EnterAction::NotEnter => {}
        }
    }
    if handle_edit_key(key, &mut ui.agents_page.composer) {
        return DeckAction::Handled;
    }

    // List navigation and verbs, from an empty composer.
    if composer_empty {
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            ui.agents_page.open = false;
            return DeckAction::Handled;
        }
        if crate::deck_ui::list_nav::select(key, &mut ui.agents_page.sel, count, true) {
            return DeckAction::Handled;
        }
        match key.code {
            KeyCode::Enter | KeyCode::Right => return open_selected(ui, &list),
            KeyCode::Char('r') if !ctrl => {
                ui.agents_page.notice = Some("refreshing sessions…".to_string());
                return DeckAction::Send(WorkspaceInput::SessionsRefresh);
            }
            // A brand-new full session, distinct from the composer's lane:
            // the driver parks this session and opens an empty record — the
            // SESSIONS overlay's own `n`, reachable from here too.
            KeyCode::Char('n') if !ctrl => {
                ui.agents_page.open = false;
                return DeckAction::Send(WorkspaceInput::SessionNew);
            }
            KeyCode::Char('x') if ctrl => return kill_selected(model, ui, &list, armed),
            _ => {}
        }
    } else if matches!(key.code, KeyCode::Esc) {
        // Esc with a draft: clear the draft first; the next Esc closes.
        ui.agents_page.composer.clear();
        return DeckAction::Handled;
    }

    // Everything else types into the page composer.
    match key.code {
        KeyCode::Char(c) if !ctrl => {
            ui.agents_page.composer.insert_char(c);
            DeckAction::Handled
        }
        KeyCode::Backspace => {
            ui.agents_page.composer.backspace();
            DeckAction::Handled
        }
        _ => DeckAction::Handled,
    }
}

/// Route one submitted line: a scoped command leaves queue-free, an
/// out-of-scope command is refused with a notice, and anything else starts a
/// new agent lane on it.
fn submit(ui: &mut DeckUi, text: String) -> DeckAction {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return DeckAction::Handled;
    }
    if trimmed.starts_with('/') {
        let head = trimmed.split_whitespace().next().unwrap_or(trimmed);
        if !PAGE_COMMANDS.contains(&head) {
            ui.agents_page.notice = Some(format!(
                "{head} is not available on this page — esc back to the session for it"
            ));
            return DeckAction::Handled;
        }
        ui.agents_page.notice = Some(format!("{head} sent — its reply prints in the transcript"));
        return DeckAction::Send(WorkspaceInput::Command {
            text: trimmed.to_string(),
        });
    }
    ui.agents_page.notice = Some(format!(
        "starting a new agent on: {}",
        crate::v2::sessions::truncate(trimmed, 60)
    ));
    DeckAction::Send(WorkspaceInput::SpawnLane {
        text: trimmed.to_string(),
    })
}

/// `⏎` on a row: a lane opens on the SESSION tab (the page closes); a
/// resumable session resumes; anything else replays read-only.
fn open_selected(ui: &mut DeckUi, list: &[Row]) -> DeckAction {
    match list.get(ui.agents_page.sel) {
        Some(Row::Lane(i)) => {
            ui.agents_page.open = false;
            ui.focus_agent(*i);
            DeckAction::Handled
        }
        Some(Row::Session(i)) => match ui.sessions.get(*i).cloned() {
            Some(s) if s.mine => {
                ui.agents_page.open = false;
                DeckAction::Handled
            }
            Some(s) if s.resumable => {
                ui.agents_page.open = false;
                DeckAction::Send(WorkspaceInput::SessionResume { id: s.id })
            }
            Some(s) => {
                ui.agents_page.open = false;
                DeckAction::Send(WorkspaceInput::SessionOpen { id: s.id })
            }
            None => DeckAction::Handled,
        },
        None => DeckAction::Handled,
    }
}

/// `⌃x` twice on a live lane stops it — the SUB-AGENTS overlay's rule. A
/// session row takes no control verb from here.
fn kill_selected(model: &WorkspaceModel, ui: &mut DeckUi, list: &[Row], armed: bool) -> DeckAction {
    match list.get(ui.agents_page.sel) {
        Some(Row::Lane(i)) => match model.agents.get(*i) {
            Some(lane) if lane.status.is_terminal() => DeckAction::Handled,
            Some(lane) if armed => DeckAction::Send(WorkspaceInput::Control {
                agent: lane.meta.id.clone(),
                control: AgentControl::Stop,
            }),
            Some(lane) => {
                ui.agents_page.kill_armed = true;
                ui.agents_page.notice = Some(format!("⌃x again stops {}", lane.meta.id));
                DeckAction::Handled
            }
            None => DeckAction::Handled,
        },
        Some(Row::Session(_)) => {
            ui.agents_page.notice =
                Some("a session row takes no kill — open it and stop it there".to_string());
            DeckAction::Handled
        }
        None => DeckAction::Handled,
    }
}

/// Draw the page over the whole frame.
pub fn render(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    Clear.render(area, buf);
    buf.set_style(area, Style::new().bg(token::BG));
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    let gold = Style::new().fg(token::GOLD);
    let inner_w = (area.width as usize).saturating_sub(2);

    let list = rows(model, ui);
    let selected = ui.agents_page.sel.min(list.len().saturating_sub(1));
    let (waiting, working, completed) = counts(model, ui);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(" AGENTS", gold.add_modifier(Modifier::BOLD)),
        Span::styled(
            format!(" · {waiting} waiting · {working} working · {completed} completed"),
            muted,
        ),
    ]));
    lines.push(Line::default());

    let mut section = None;
    for (i, row) in list.iter().enumerate() {
        let live = match row {
            Row::Lane(l) => model
                .agents
                .get(*l)
                .is_some_and(|a| !a.status.is_terminal()),
            Row::Session(s) => ui.sessions.get(*s).is_some_and(|s| is_live(s.phase)),
        };
        let heading = if live { "WORKING" } else { "COMPLETED" };
        if section != Some(heading) {
            section = Some(heading);
            lines.push(Line::from(Span::styled(format!(" {heading}"), dim)));
        }
        let is_sel = i == selected;
        let cursor = Span::styled(if is_sel { " ▸ " } else { "   " }.to_string(), gold);
        match row {
            Row::Lane(l) => {
                let Some(lane) = model.agents.get(*l) else {
                    continue;
                };
                lines.push(lane_head(lane, model, cursor, is_sel, inner_w));
                lines.push(Line::from(vec![
                    Span::raw("     "),
                    Span::styled(
                        crate::v2::subagents::purpose(&lane.meta).to_string(),
                        if is_sel { text } else { muted },
                    ),
                ]));
            }
            Row::Session(s) => {
                let Some(session) = ui.sessions.get(*s) else {
                    continue;
                };
                lines.push(session_head(session, model.now_ms, cursor, is_sel, inner_w));
            }
        }
    }
    if list.is_empty() {
        lines.push(Line::from(Span::styled(
            "   no agents yet — describe a task below to start one",
            muted,
        )));
    }

    // Bottom chrome: notice, composer, hints — pinned to the frame's foot.
    let c_layout = crate::composer::layout(&ui.agents_page.composer, inner_w.saturating_sub(3));
    let composer_h = c_layout.rows.len().clamp(1, 3) as u16;
    let foot_h = composer_h + 2;
    let body_h = area.height.saturating_sub(foot_h);
    let body = Rect {
        height: body_h,
        ..area
    };
    Paragraph::new(lines.into_iter().take(body_h as usize).collect::<Vec<_>>()).render(body, buf);

    let mut foot: Vec<Line<'static>> = Vec::new();
    if let Some(notice) = &ui.agents_page.notice {
        foot.push(Line::from(Span::styled(format!(" {notice}"), gold)));
    } else {
        foot.push(Line::default());
    }
    for (r, row) in c_layout.rows.iter().enumerate() {
        let prefix = if r == 0 { " ❯ " } else { "   " };
        if r == 0 && row.is_empty() && ui.agents_page.composer.is_blank() {
            foot.push(Line::from(vec![
                Span::styled(prefix.to_string(), gold),
                Span::styled("describe a task for a new session".to_string(), dim),
            ]));
        } else {
            foot.push(Line::from(vec![
                Span::styled(prefix.to_string(), gold),
                Span::styled(row.clone(), text),
            ]));
        }
    }
    foot.push(Line::from(Span::styled(
        "   ⏎ start a new agent · / commands · ↑↓ select · ⏎ open · n new session · ⌃x⌃x kill · r refresh · esc back",
        dim,
    )));
    let foot_area = Rect {
        y: area.y + body_h,
        height: foot_h.min(area.height),
        ..area
    };
    Paragraph::new(foot).render(foot_area, buf);

    // The page's popups, anchored above the composer: the scoped slash menu,
    // or `/model`'s argument menu.
    let arg = model_arg_matches(model, ui);
    let menu: Vec<String> = if arg.is_empty() {
        slash_matches(ui)
    } else {
        arg
    };
    if !menu.is_empty() {
        render_menu(ui, &menu, area, foot_area.y + 1, buf);
    }
}

/// A lane's head row: id, status, clock, model, spend.
fn lane_head(
    lane: &AgentEntry,
    model: &WorkspaceModel,
    cursor: Span<'static>,
    is_sel: bool,
    inner_w: usize,
) -> Line<'static> {
    let muted = Style::new().fg(token::MUTED);
    let mut style = Style::new().fg(token::TEXT);
    if is_sel {
        style = style.bg(token::HL).add_modifier(Modifier::BOLD);
    }
    let mut parts = vec![
        lane.status.label().to_string(),
        cards::fmt_mss(lane.elapsed_ms(model.now_ms)),
    ];
    if let Some(m) = &lane.meta.model {
        parts.push(m.clone());
    }
    parts.push(format!("${:.2}", lane.cost_usd));
    let head = format!("◆ {}", lane.meta.id);
    Line::from(vec![
        cursor,
        Span::styled(
            crate::v2::sessions::truncate(&head, inner_w.saturating_sub(30)),
            style,
        ),
        Span::styled(format!("  {}", parts.join(" · ")), muted),
    ])
}

/// A session's head row: phase glyph, title, provenance tag.
fn session_head(
    session: &SessionInfo,
    now_ms: u64,
    cursor: Span<'static>,
    is_sel: bool,
    inner_w: usize,
) -> Line<'static> {
    let (mark, metal) = crate::v2::sessions::phase_mark(session.phase);
    let mut style = Style::new().fg(token::TEXT);
    if is_sel {
        style = style.bg(token::HL).add_modifier(Modifier::BOLD);
    }
    let age = crate::v2::sessions::fmt_age(now_ms.saturating_sub(session.updated_ms));
    let tag = if session.mine {
        "this session".to_string()
    } else if session.resumable {
        format!("↩ resume · {age}")
    } else {
        age
    };
    Line::from(vec![
        cursor,
        Span::styled(format!("{mark} "), Style::new().fg(metal)),
        Span::styled(
            crate::v2::sessions::truncate(&session.title, inner_w.saturating_sub(tag.len() + 8)),
            style,
        ),
        Span::styled(format!("  {tag}"), Style::new().fg(token::DIM)),
    ])
}

/// The page's popup: a plain selected-row list drawn just above the
/// composer. Small on purpose — the vocabulary here is a handful of
/// commands or model specs, not the deck's whole palette.
fn render_menu(ui: &DeckUi, menu: &[String], area: Rect, anchor_y: u16, buf: &mut Buffer) {
    let rows = menu.len().min(8) as u16;
    let y = anchor_y.saturating_sub(rows).max(area.y);
    let popup = Rect {
        x: area.x + 3,
        y,
        width: area.width.saturating_sub(6).min(60),
        height: rows.min(area.height),
    };
    Clear.render(popup, buf);
    let selected = ui.agents_page.menu_selected.min(menu.len() - 1);
    let lines: Vec<Line<'static>> = menu
        .iter()
        .take(rows as usize)
        .enumerate()
        .map(|(i, name)| {
            let mut style = Style::new().fg(token::TEXT);
            if i == selected {
                style = style.bg(token::HL).add_modifier(Modifier::BOLD);
            }
            Line::from(Span::styled(format!(" {name} "), style))
        })
        .collect();
    Paragraph::new(lines)
        .style(Style::new().bg(token::BG))
        .render(popup, buf);
}
