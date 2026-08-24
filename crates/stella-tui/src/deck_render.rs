//! The top-level deck frame: the tab bar + the active view + an always-on
//! composer + a status bar, with the splash as a full-frame overlay until it
//! finishes. This is the tab dispatcher and the one place the deck's chrome is
//! drawn.
//!
//! Every band drawn here — chrome row, active-tab content, floating overlay —
//! goes through `panel_guard`, so a panic inside one view becomes an
//! error card in that band instead of unwinding out of `terminal.draw` and
//! ending the session.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use unicode_width::UnicodeWidthChar;

use crate::cache_panel;
use crate::composer::{ComposerLayout, PaletteState, layout as composer_layout, split_row_at};
use crate::deck::{DeckTab, WorkspaceModel};
use crate::deck_ui::{DeckUi, InstalledMode, IssuesMode};
use crate::panel_guard::{guarded_band, guarded_overlay};
use crate::render::{render_arg_popup, render_slash_popup, scroll_window_start, slash_popup_area};
use crate::{notice, splash, theme, views};

/// The accent prompt prefix on every composer row. Chrome, not content — it
/// is never part of the submitted string and the caret cannot enter it.
const PROMPT_PREFIX: &str = ">>> ";
/// Display width of [`PROMPT_PREFIX`].
const PROMPT_PREFIX_W: usize = 4;
/// One reserved column on the composer's right for the scroll indicator.
const COMPOSER_GUTTER_W: usize = 1;

pub fn render_deck(model: &WorkspaceModel, ui: &mut DeckUi, frame: &mut Frame) {
    let area = frame.area();
    let buf = frame.buffer_mut();

    // The ground is a real frame fill, not an assumption about the user's
    // terminal background — the deck looks the same over a white terminal as
    // over a black one. The base FOREGROUND is filled for the same reason:
    // prose that sets no colour of its own must still be a real theme tone
    // (INK/`TEXT_PRIMARY`), never the terminal default — otherwise the
    // `stella-light` remap can't turn it to ink and body text would render
    // white-on-white on paper. `degrade_buffer` narrows both per colour depth,
    // and NO_COLOR strips them entirely (structure survives).
    buf.set_style(
        area,
        Style::default().bg(theme::GROUND).fg(theme::TEXT_PRIMARY),
    );

    // The splash owns the whole frame until it finishes / is skipped.
    if !ui.splash.is_done() {
        let splash_model = model.latest_model();
        guarded_band(buf, area, "splash", |b| {
            splash::render(&ui.splash, splash_model, area, b)
        });
        return;
    }

    // The AGENTS page (`←` twice) owns the whole frame while open — a page,
    // not a popup, so it replaces the bands rather than floating over them.
    // The parked asks, the startup notice, and help still stack above it,
    // exactly as they stack over the normal bands below.
    if ui.agents_page.open {
        guarded_band(buf, area, "agents page", |b| {
            crate::v2::agents_page::render(model, ui, area, b)
        });
        parked::render(ui, area, buf);
        if ui.notice.is_visible() {
            guarded_overlay(buf, area, "notice", |b| notice::render(&ui.notice, area, b));
        }
        if ui.help_open {
            guarded_overlay(buf, area, "help", |b| render_help(model, ui, area, b));
        }
        return;
    }

    // SPEC 5, top to bottom: tab row (one row, the breadcrumb on SESSION) |
    // content | composer | keybinding hint row | status bar. `v2::frame`
    // carries the account of the v1 chrome this replaced. The composer grows
    // with its soft-wrapped content up to a cap, then scrolls to keep the
    // cursor visible; its text width is the frame minus the 4-column `>>> `
    // prefix and the 1-column scroll gutter.
    let text_w = (area.width as usize).saturating_sub(PROMPT_PREFIX_W + COMPOSER_GUTTER_W);
    let c_layout = composer_layout(&ui.composer, text_w.max(1));
    let composer_h = c_layout.rows.len().clamp(1, DECK_COMPOSER_MAX_ROWS) as u16;
    // One row (SPEC 5), and a second only for an earned low-hit-rate
    // diagnosis (#267). `v2::status_bar` carries the argument for why the
    // two-row labelled band is gone.
    let has_diagnosis = model
        .agents
        .get(ui.focused)
        .and_then(|a| a.cache_diagnosis(cache_panel::LOW_HIT_RATE_THRESHOLD))
        .is_some();
    let statline_h = if has_diagnosis { 2 } else { 1 };
    let bands = Layout::vertical([
        Constraint::Length(1),          // tab row / breadcrumb
        Constraint::Min(1),             // active view
        Constraint::Length(1),          // air above the prompt, or the pulse row
        Constraint::Length(composer_h), // composer
        Constraint::Length(1),          // keybinding hint row
        Constraint::Length(statline_h), // status bar (+ diagnosis)
    ])
    .split(area);

    let tab = ui.tab;
    guarded_band(buf, bands[0], "tab bar", |b| {
        crate::v2::frame::render_tab_row(model, ui, bands[0], b)
    });

    let content = bands[1];
    guarded_band(buf, content, tab.title(), |b| match tab {
        DeckTab::Session => crate::v2::session::render(model, ui, content, b),
        DeckTab::Agents => crate::v2::installed::render(ui, model.now_ms, content, b),
        DeckTab::Traces => views::traces::render(model, ui, content, b),
        DeckTab::Graph => views::graph::render(model, ui, content, b),
        DeckTab::Files => views::files::render(model, ui, content, b),
        DeckTab::Skills => crate::v2::skills::render(model, ui, content, b),
        DeckTab::Mcp => crate::v2::mcp_tab::render(model, ui, content, b),
        DeckTab::Issues => {
            crate::v2::issues_tab::render(model.pr.as_ref(), &ui.issues, ui.accessible, content, b)
        }
        DeckTab::Settings => views::settings::render(model, ui, content, b),
    });

    // The pulse row (`v2::pulse`) draws in the air row off the SESSION tab,
    // and at an opened lane: the live agent's status, quiet time, place and
    // last words, so no tab is blind to the turn.
    guarded_band(buf, bands[2], "pulse", |b| {
        crate::v2::pulse::render_row(model, ui, bands[2], b)
    });
    guarded_band(buf, bands[3], "composer", |b| {
        render_composer(&c_layout, bands[3], b)
    });
    guarded_band(buf, bands[4], "hints", |b| {
        crate::v2::frame::render_hint_row(model, ui, bands[4], b)
    });
    guarded_band(buf, bands[5], "statline", |b| {
        crate::v2::status_bar::render_band(model, ui, bands[5], b)
    });
    let composer_cursor = composer_cursor_position(&c_layout, bands[3]);

    // Floating popups sit above the chrome: the slash menu anchors to the
    // composer; the queue editor centers over the content.
    let slash = ui
        .composer
        .slash_menu(&ui.slash_commands, &palette_state(model, ui));
    let slash_open = slash.as_ref().is_some_and(|m| !m.is_empty());
    if let Some(menu) = slash.filter(|m| !m.is_empty()) {
        let selected = ui.slash_selected.min(menu.matches.len().saturating_sub(1));
        let popup = slash_popup_area(area, bands[3], crate::render::display_rows(&menu).len());
        // Live values in descriptions, read from the model at render time —
        // never cached in `DeckUi` (D3).
        let live = slash_live_hints(model, ui);
        guarded_band(buf, popup, "slash menu", |b| {
            render_slash_popup(&menu, selected, &live, popup, b)
        });
    }
    // The `/model` argument menu opens where the slash menu closed — the
    // buffer is `/model` plus an in-progress argument (`composer::args`).
    let arg_matches = crate::composer::args::arg_matches(
        &ui.composer,
        "/model",
        &crate::views::picker::typeahead_candidates(model, ui),
    );
    let arg_open = !arg_matches.is_empty();
    if arg_open {
        let fragment = ui
            .composer
            .buffer()
            .strip_prefix("/model ")
            .unwrap_or_default()
            .to_string();
        let selected = ui.slash_selected.min(arg_matches.len() - 1);
        let popup = slash_popup_area(area, bands[3], arg_matches.len());
        guarded_band(buf, popup, "model menu", |b| {
            render_arg_popup("/model", &arg_matches, &fragment, selected, popup, b)
        });
    }
    if ui.queue_open {
        guarded_overlay(buf, area, "queue", |b| {
            crate::v2::queue::render(model, ui, area, b)
        });
    }
    // The STATE overlay (`⌃s`): the expansion of the Session tab's one-row
    // state strip. Drawn from the focused agent, so it says nothing at all
    // when there is no agent to say it about.
    if ui.graph_picker_open {
        guarded_overlay(buf, area, "graph picker", |b| {
            render_graph_picker(ui, area, b)
        });
    }
    // The transcript-page overlays (SESSIONS / INBOX / CONTEXT) center over
    // the whole frame like the queue editor; help (below) still wins the top.
    if ui.sessions_open {
        guarded_overlay(buf, area, "sessions", |b| {
            crate::v2::sessions::render(model, ui, area, b)
        });
    }
    if ui.subagents.open {
        guarded_overlay(buf, area, "sub-agents", |b| {
            crate::v2::subagents::render(model, ui, area, b)
        });
    }
    if ui.inbox_open {
        guarded_overlay(buf, area, "inbox", |b| {
            render_inbox_overlay(model, ui, area, b)
        });
    }
    if ui.context_open {
        guarded_overlay(buf, area, "context", |b| {
            render_context_overlay(model, ui, area, b)
        });
    }
    if ui.inspect_open {
        guarded_overlay(buf, area, "inspect", |b| {
            render_inspect_overlay(ui, area, b)
        });
    }
    // The floating cards (`/plan` · `/models` · `/budget`): at most one is up
    // (`CardState::raise` lowers the rest); help and the startup notice still
    // win the top of the stack below.
    if let Some(card) = ui.cards.open {
        use crate::deck_ui::cards::Card;
        match card {
            Card::Plan => guarded_overlay(buf, area, "plan card", |b| {
                crate::v2::plan_card::render(model, ui, area, b)
            }),
            Card::Models => guarded_overlay(buf, area, "models card", |b| {
                crate::v2::models_card::render(model, ui, area, b)
            }),
            Card::Budget => guarded_overlay(buf, area, "budget card", |b| {
                crate::v2::budget_card::render(model, ui, area, b)
            }),
        }
    }
    // (The former ENGINE overlay is gone: the engine panel is the full-width
    // body of the SETTINGS tab — see `views::settings::render`.)

    // The session-override pickers (`/model`, `/agent`): modal cards like
    // the floating ones above; the parked asks still win the top.
    if ui.model_picker.open {
        guarded_overlay(buf, area, "model picker", |b| {
            views::picker::render_model(model, ui, area, b)
        });
    }
    if ui.agent_picker.open {
        guarded_overlay(buf, area, "agent picker", |b| {
            views::picker::render_agent(ui, area, b)
        });
    }

    // The parked asks (#4220, #4240) — see `parked` for the stacking rule.
    parked::render(ui, area, buf);

    // Startup system notifications: a transient dialog over the deck, drawn
    // last but one so help — which the user asked for — still wins the top.
    // It is a no-op once dismissed or expired — asked here rather than left to
    // `notice::render`'s own early return, so the guard's scratch copy is paid
    // for only on the frames a notice is actually up.
    if ui.notice.is_visible() {
        guarded_overlay(buf, area, "notice", |b| notice::render(&ui.notice, area, b));
    }

    if ui.help_open {
        guarded_overlay(buf, area, "help", |b| render_help(model, ui, area, b));
    }

    // Position the hardware cursor at the composer caret so the terminal (and
    // anything anchored to it — CJK/IME candidate windows, screen readers,
    // cursor-following tmux panes) has something to track. Suppressed under
    // any overlay that owns the keyboard ahead of the composer — matching the
    // precedence `handle_key` already applies — so the caret doesn't sit in
    // the composer while the user is typing into a dialog. The startup
    // notice is deliberately excluded: it is non-modal, and a key still
    // reaches the composer while it's showing (see `handle_key`).
    let overlay_owns_keyboard = ui.help_open
        // Both parked asks are claimed ahead of everything else in
        // `handle_key_inner`, so either owns the keyboard while it is up.
        || parked::owns_keyboard(ui)
        || ui.queue_open
        || ui.graph_picker_open
        || ui.subagents.open
        || ui.sessions_open
        || ui.inbox_open
        || ui.context_open
        || ui.inspect_open
        // The floating cards and the session-override pickers are modal
        // while up (their handlers own every key ahead of the composer —
        // see `deck_ui::cards` / `deck_ui::pickers`).
        || ui.cards.is_open()
        || crate::deck_ui::pickers::owns_keyboard(ui)
        || slash_open
        // The routing card holds the user's words and owns every key until
        // they say where they go — it is checked first in `handle_deck_key`.
        || ui.pending_dispatch.is_some()
        // The `/model` argument menu is a popup like the slash menu above.
        || arg_open
        // The INSTALLED AGENTS sub-modes (editor / create flow / version
        // picker) are modal text inputs while open.
        || ui.installed.mode != InstalledMode::Browse
        // The ISSUES tab's sub-modes (search / create form / comment /
        // set-status) are modal while the tab is active.
        || (ui.tab == DeckTab::Issues && ui.issues.mode != IssuesMode::Browse)
        // The transcript search bar is a modal text input while up.
        || ui.search.open
        // The SETTINGS tab's ENGINE / TOOLS config editors own the keyboard
        // (inline edit buffers, model/picker filters) while focused.
        || (ui.tab == DeckTab::Settings && ui.engine.focused)
        || (ui.tab == DeckTab::Settings && ui.tools.focused);
    if !overlay_owns_keyboard && let Some(pos) = composer_cursor {
        frame.set_cursor_position(pos);
    }
}

/// The slash popup's live description overrides, read from the model at
/// render time so a row like `/inbox` carries its current unread count.
/// Recomputed per frame — never cached in `DeckUi` (D3).
/// What the session is doing, as the command palette needs to see it
/// (#4338).
///
/// Read from the model and the deck at render time and never cached, the
/// same discipline [`slash_live_hints`] follows: a relevance block computed
/// once at session start would be advice about a turn that has since ended.
///
/// The key handler calls this too — the palette's selection index means a
/// position in the ordered match list, so the renderer and the handler have
/// to order it from the same facts or the highlighted row and the dispatched
/// command drift apart.
pub(crate) fn palette_state(model: &WorkspaceModel, ui: &DeckUi) -> PaletteState {
    let agent = model.agents.get(ui.focused);
    PaletteState {
        turn_running: agent.is_some_and(|a| a.status == crate::AgentStatus::Running),
        plan_steps: agent.map_or(0, |a| a.model.plan.steps().len()),
        subagents: model.subagent_count(),
        unread: ui.notifications.iter().filter(|n| !n.read).count(),
        changed_files: model.ledger.records.len(),
        graph_missing: ui.graph.is_none(),
    }
}

fn slash_live_hints(model: &WorkspaceModel, ui: &DeckUi) -> Vec<(String, String)> {
    let mut hints = Vec::new();
    let unread = ui.notifications.iter().filter(|n| !n.read).count();
    if unread > 0 {
        hints.push(("/inbox".to_string(), format!("{unread} unread")));
    }
    if let Some(agent) = model.agents.get(ui.focused) {
        let plan = &agent.model.plan;
        if !plan.is_empty() {
            let (done, total) = plan.progress();
            hints.push((
                "/plan".to_string(),
                format!("{} — {done} of {total} steps done", plan.state.label()),
            ));
        }
    }
    match model.budget_cap_usd {
        Some(cap) => hints.push((
            "/budget".to_string(),
            format!("run ${:.2} of ${cap:.2} · edit the cap", model.total_cost()),
        )),
        None => hints.push((
            "/budget".to_string(),
            format!("run ${:.2} · set a spend cap", model.total_cost()),
        )),
    }
    hints
}

// The queue editor popup lives in `crate::v2::queue`.

/// The INBOX overlay (`/inbox`): the persist-until-read notifications,
/// newest first — unread bold with a ● dot, read dimmed with ✓, and a `↗`
/// marker on rows that link a session (⏎ marks those read AND opens the
/// session). Marking read (⏎/Space, or `R` for all) is the only way a
/// message leaves the badge.
fn render_inbox_overlay(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    let w = area.width.saturating_sub(8).min(96);
    let h = area.height.saturating_sub(6).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    Clear.render(popup, buf);

    let unread = ui.notifications.iter().filter(|n| !n.read).count();
    let selected = ui.inbox_sel.min(ui.notifications.len().saturating_sub(1));
    let mut lines: Vec<Line<'static>> = Vec::new();

    if ui.notifications.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "  inbox zero — notifications persist here until read",
            theme::muted(),
        )));
    }

    let visible = ((h as usize).saturating_sub(4) / 2).max(1);
    let start = selected
        .saturating_sub(visible.saturating_sub(1) / 2)
        .min(ui.notifications.len().saturating_sub(visible));
    for (i, n) in ui
        .notifications
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
    {
        let is_sel = i == selected;
        let marker = if is_sel { "▸ " } else { "  " };
        let (dot, mut title_style) = if n.read {
            ("✓ ", theme::muted())
        } else {
            (
                "● ",
                Style::default().fg(theme::INK).add_modifier(Modifier::BOLD),
            )
        };
        if is_sel {
            title_style = title_style.bg(theme::SELECT_BG);
        }
        let dot_style = if n.read {
            theme::muted()
        } else {
            Style::default().fg(theme::WARNING_BRIGHT)
        };
        let mut row = vec![
            Span::raw(marker),
            Span::styled(dot, dot_style),
            Span::styled(
                truncate_chars(&n.title, (w as usize).saturating_sub(10)),
                title_style,
            ),
        ];
        if n.session_id.is_some() {
            // A subtle link marker: ⏎ on this row opens the session it is
            // about (replaying it when it is no longer live).
            row.push(Span::styled(" ↗", theme::muted()));
        }
        lines.push(Line::from(row));
        let source = if n.source.is_empty() {
            String::new()
        } else {
            format!(" · {}", n.source)
        };
        let detail = format!(
            "      {}{} · {}",
            truncate_chars(&n.body, (w as usize).saturating_sub(24)),
            source,
            fmt_age(model.now_ms.saturating_sub(n.created_ms)),
        );
        lines.push(Line::from(Span::styled(
            truncate_chars(&detail, (w as usize).saturating_sub(4)),
            theme::muted(),
        )));
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " ↑/↓ select · ↵ open · ␣ mark read · R mark all read · esc close",
        theme::muted(),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::accent())
        .title(format!(" inbox · {unread} unread "));
    Paragraph::new(lines).block(block).render(popup, buf);
}

/// The CONTEXT overlay (empty-prompt `→`, `/context`): what THIS session is
/// running with — the active skills and the MCP servers — without leaving
/// the transcript. Read-only; management stays on the SKILLS/MCP tabs.
fn render_context_overlay(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    let w = area.width.saturating_sub(8).min(96);
    let h = area.height.saturating_sub(6).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    Clear.render(popup, buf);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let skills = &ui.skills.view.rows;
    let enabled_skills = skills.iter().filter(|s| s.enabled).count();
    lines.push(Line::from(Span::styled(
        format!("  ACTIVE SKILLS ({enabled_skills}/{})", skills.len()),
        theme::accent().add_modifier(Modifier::BOLD),
    )));
    if skills.is_empty() {
        lines.push(Line::from(Span::styled(
            "    none installed — /skills to browse",
            theme::muted(),
        )));
    }
    for skill in skills {
        let (glyph, glyph_style) = if skill.enabled {
            ("●", Style::default().fg(theme::SUCCESS_BRIGHT))
        } else {
            ("○", theme::muted())
        };
        let desc = truncate_chars(&skill.description, (w as usize).saturating_sub(30));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(glyph, glyph_style),
            Span::raw(" "),
            Span::styled(skill.name.clone(), Style::default().fg(theme::INK)),
            Span::styled(format!("  [{}]", skill.origin), theme::muted()),
            Span::styled(format!("  {desc}"), theme::muted()),
        ]));
    }

    lines.push(Line::default());
    let servers = &ui.mcp.servers;
    let connected = servers.iter().filter(|s| s.connected).count();
    lines.push(Line::from(Span::styled(
        format!("  MCP SERVERS ({connected}/{} connected)", servers.len()),
        theme::accent().add_modifier(Modifier::BOLD),
    )));
    if servers.is_empty() {
        lines.push(Line::from(Span::styled(
            "    none configured — /mcp to search + install",
            theme::muted(),
        )));
    }
    for server in servers {
        let (glyph, glyph_style) = if server.enabled && server.connected {
            ("●", Style::default().fg(theme::SUCCESS_BRIGHT))
        } else if server.enabled {
            ("◌", Style::default().fg(theme::WARNING_BRIGHT))
        } else {
            ("○", theme::muted())
        };
        let state = if !server.enabled {
            "disabled".to_string()
        } else if server.connected {
            server.health.clone().unwrap_or_else(|| "live".to_string())
        } else {
            "not connected".to_string()
        };
        let heading = crate::v2::mcp_tab::compact_heading(server);
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(glyph, glyph_style),
            Span::raw(" "),
            Span::styled(heading, Style::default().fg(theme::INK)),
            Span::styled(format!("  [{}]", server.kind), theme::muted()),
            Span::styled(format!("  {state}"), theme::muted()),
            Span::styled(format!("  · {} tools", server.tool_count), theme::muted()),
        ];
        match server.oauth {
            Some(true) => spans.push(Span::styled(
                "  ⚿ oauth ✓",
                Style::default().fg(theme::SUCCESS),
            )),
            Some(false) => spans.push(Span::styled("  ⚿ no oauth login", theme::muted())),
            None => {}
        }
        lines.push(Line::from(spans));
    }

    // The session vitals that left the statline (D1): cache volumes, the
    // warmth countdown, engine state. Diagnostics, not glanceables — this
    // overlay is where a reader who wants them looks.
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "  SESSION VITALS",
        theme::accent().add_modifier(Modifier::BOLD),
    )));
    let dim = Style::default().fg(theme::TEXT_TERTIARY);
    let focused = model.agents.get(ui.focused);
    lines.push(Line::from(vec![
        Span::styled("  cache ", dim),
        Span::styled(
            cache_panel::cache_volumes(
                model.cache_hit_tokens(),
                model.total_cache_write_tokens(),
                model.total_input_tokens(),
            ),
            Style::default().fg(theme::INK),
        ),
        Span::styled("  ·  warmth ", dim),
        Span::styled(
            cache_panel::fmt_warmth(focused.and_then(|a| a.cache_warmth_secs(model.now_ms))),
            Style::default().fg(theme::INK),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  engine ", dim),
        Span::styled(
            format!("{} active", model.active_count()),
            Style::default().fg(theme::INK),
        ),
    ]));

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " ↑/↓ scroll · manage on the SKILLS / MCP tabs · esc/→ close",
        theme::muted(),
    )));

    // Clamp the scroll to the measured content so ↓ can't run off the end.
    let inner_h = (h as usize).saturating_sub(2);
    let max_scroll = lines.len().saturating_sub(inner_h);
    ui.context_scroll = ui.context_scroll.min(max_scroll);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::accent())
        .title(" session context ");
    Paragraph::new(lines)
        .block(block)
        .scroll((u16::try_from(ui.context_scroll).unwrap_or(u16::MAX), 0))
        .render(popup, buf);
}

/// The INSPECT overlay (`⌃g`): the model calls this execution recorded, and
/// the reconstructed context of the one selected — what was actually sent,
/// system prompt included, rebuilt from the receipts rather than from any live
/// UI state. Two modes in one popup, keyed off `ui.inspect_view`.
fn render_inspect_overlay(ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    let w = area.width.saturating_sub(6).min(120);
    let h = area.height.saturating_sub(4).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    Clear.render(popup, buf);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let title;

    if ui.inspect_pending && ui.inspect_view.is_none() {
        title = " inspect · reconstructing ";
        lines.push(Line::from(Span::styled(
            "  reconstructing the call's context from the recorded receipt…",
            theme::muted(),
        )));
    } else if let Some(view) = ui.inspect_view.as_ref() {
        // Borrowed, never cloned and never moved out: a reconstructed context
        // is a whole prompt (up to a full window of messages) and the deck
        // redraws on a ~30 fps tick, so cloning it here copied megabytes per
        // second for a read-only walk. A take-and-restore avoided the clone
        // but not the tear — a panic between the two lost the view for good
        // (see `crate::panel_guard`); a shared borrow avoids both.
        title = " inspect · context sent ";
        let call = &view.call;
        lines.push(Line::from(Span::styled(
            format!(
                "  turn {} · step {} · call-seq {} · {}",
                call.turn_instance, call.step, call.call_seq, call.call_role
            ),
            theme::accent().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!(
                "  {} / {} · {} message(s)",
                call.provider,
                call.model,
                view.messages.len()
            ),
            theme::muted(),
        )));
        // Never merged: unresolved is a coverage gap, a mismatch means the
        // recovered bytes are not this block's. The gap is never phrased as
        // tampering — it is a documented coverage boundary, not a signal.
        if view.unresolved > 0 {
            lines.push(Line::from(Span::styled(
                format!(
                    "  ! {} block(s) unresolved — synthetic results, discarded speculation, \
                     or attachments",
                    view.unresolved
                ),
                Style::default().fg(theme::WARN),
            )));
        }
        // A mismatch means one of two things depending on who wrote the
        // journal, and `InspectView` (not this renderer) holds that verdict —
        // see `envelope::InspectView::digest_mismatch_line`, which also keeps
        // both variants short enough to survive this overlay's clip (#1981).
        if let Some((text, alarm)) = view.digest_mismatch_line() {
            let tone = if alarm { theme::DANGER } else { theme::WARN };
            lines.push(Line::from(Span::styled(text, Style::default().fg(tone))));
        }
        if view.verified {
            lines.push(Line::from(Span::styled(
                "  verified · every journal-resolved block re-hashed to its recorded digest",
                Style::default().fg(theme::SUCCESS),
            )));
        }
        let body_width = (w as usize).saturating_sub(6);
        for (index, message) in view.messages.iter().enumerate() {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                format!("  ─── [{index}] {} ───", message.role),
                theme::accent().add_modifier(Modifier::BOLD),
            )));
            // A message the driver could break down by provenance — the system
            // prefix — renders as its sections instead of one wall, each headed
            // by the setting that produced it. The bodies concatenate to
            // `content`, so nothing is hidden by taking this branch; a message
            // with no breakdown takes the flat path it always did.
            if message.sections.is_empty() {
                push_wrapped(&mut lines, &message.content, body_width);
                continue;
            }
            for section in &message.sections {
                lines.push(Line::from(Span::styled(
                    format!("    ┌ {}", section.label),
                    theme::accent(),
                )));
                let attribution = format!("from: {}", section.source);
                for chunk in wrap_chars(&attribution, body_width.saturating_sub(2)) {
                    lines.push(Line::from(Span::styled(
                        format!("    │ {chunk}"),
                        theme::muted(),
                    )));
                }
                push_wrapped(&mut lines, &section.body, body_width);
            }
        }
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            " ↑/↓ pgup/pgdn scroll · esc/← back to calls · q close",
            theme::muted(),
        )));
    } else {
        title = " inspect · recorded calls ";
        lines.push(Line::from(Span::styled(
            "  every model call this execution recorded a receipt for",
            theme::muted(),
        )));
        lines.push(Line::default());
        if ui.inspect_calls.is_empty() {
            lines.push(Line::from(Span::styled(
                "    no receipts for this execution yet — run a turn, then reopen",
                theme::muted(),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "    TURN  STEP  SEQ  ROLE            PROVIDER    MODEL",
                theme::muted(),
            )));
        }
        for (index, call) in ui.inspect_calls.iter().enumerate() {
            let selected = index == ui.inspect_sel;
            let style = if selected {
                theme::accent().add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::INK)
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "  {} {:>4}  {:>4}  {:>3}  {:<14}  {:<10}  {}",
                    if selected { "›" } else { " " },
                    call.turn_instance,
                    call.step,
                    call.call_seq,
                    truncate_chars(&call.call_role, 14),
                    truncate_chars(&call.provider, 10),
                    truncate_chars(&call.model, 22),
                ),
                style,
            )));
        }
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            " ↑/↓ select · ⏎ show the context it was sent · r refresh · esc close",
            theme::muted(),
        )));
        // The whole popup scrolls by `inspect_scroll`, and the list-mode key
        // handler only moves `inspect_sel` — it never touches the scroll. Track
        // the selection here so the `›` row stays on-screen when the call list
        // is taller than the popup. The rows are preceded by three fixed lines
        // (title · blank · header), so the selected row is at `3 + inspect_sel`.
        let inner_h = (h as usize).saturating_sub(2);
        let sel_line = 3 + ui.inspect_sel;
        ui.inspect_scroll = scroll_window_start(lines.len(), sel_line, inner_h);
    }

    // Clamp the scroll to the measured content so ↓ can't run off the end.
    let inner_h = (h as usize).saturating_sub(2);
    let max_scroll = lines.len().saturating_sub(inner_h);
    ui.inspect_scroll = ui.inspect_scroll.min(max_scroll);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::accent())
        .title(title);
    // Saturate, never `as`-cast: a reconstructed context can run past 65 535
    // lines, and a wrapped u16 would snap the overlay back to the top.
    let scroll_row = u16::try_from(ui.inspect_scroll).unwrap_or(u16::MAX);
    Paragraph::new(lines)
        .block(block)
        .scroll((scroll_row, 0))
        .render(popup, buf);
}

/// Push a multi-line body into the INSPECT overlay's line buffer, hard-wrapped
/// and indented.
///
/// Wrapped by hand rather than by `Paragraph`: the overlay scrolls by row
/// offset, so its own wrap would make rows stop equalling lines and the scroll
/// clamp would lie about how far there is left to go.
fn push_wrapped(lines: &mut Vec<Line<'static>>, body: &str, width: usize) {
    for raw in body.lines() {
        for chunk in wrap_chars(raw, width) {
            lines.push(Line::from(Span::styled(
                format!("    {chunk}"),
                Style::default().fg(theme::INK),
            )));
        }
    }
}

/// Hard-wrap one logical line to `width` characters, char-safe. Returns at
/// least one (possibly empty) chunk so a blank source line still occupies a row
/// — the scroll clamp counts rows, so dropping blanks would desync it.
fn wrap_chars(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars
        .chunks(width)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

/// Display-width-safe prefix truncation with an ellipsis. `max_cols` is a
/// terminal column budget, not a character count — a char-counting truncation
/// would under-truncate double-width glyphs (CJK, emoji) by up to 2×,
/// overflowing the caller's fixed-width row. Content here (session titles,
/// notification bodies, skill descriptions) is agent- or user-authored text,
/// not guaranteed ASCII.
fn truncate_chars(s: &str, max_cols: usize) -> String {
    if display_width(s) <= max_cols {
        return s.to_string();
    }
    // Leave one column for the ellipsis glyph itself.
    let budget = max_cols.saturating_sub(1);
    let mut head = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > budget {
            break;
        }
        head.push(ch);
        w += cw;
    }
    format!("{head}…")
}

/// Terminal column width of `s` (unicode-width aware — CJK and most emoji are
/// two columns wide, unlike a plain `chars().count()`).
fn display_width(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// A compact "3m ago"-style age from a millisecond delta.
fn fmt_age(delta_ms: u64) -> String {
    let secs = delta_ms / 1000;
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

/// The Graph tab's file picker: a centered overlay listing every indexed file,
/// narrowed by a filter-as-you-type query, with the selection highlighted and
/// windowed so it stays in view on long lists (the shared
/// [`scroll_window_start`] the slash popup uses). Selecting a row re-roots the
/// neighborhood on that file; the current focus opens pre-selected as the
/// sensible default.
fn render_graph_picker(ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    let Some(graph) = ui.graph.as_ref() else {
        return;
    };
    let matches = graph.matching_files(&ui.graph_picker_query);

    let w = area.width.min(64);
    let h = area.height.min(18);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    Clear.render(popup, buf);

    // Query line (top) + legend line (bottom) + two borders bracket the rows.
    let inner_h = (h as usize).saturating_sub(2);
    let visible_rows = inner_h.saturating_sub(2).max(1);
    let selected = ui.graph_picker_sel.min(matches.len().saturating_sub(1));
    let first = scroll_window_start(matches.len(), selected, visible_rows);
    let last = (first + visible_rows).min(matches.len());

    // The filter query, with a violet caret so the keybind/edit accent reads.
    let mut lines: Vec<Line<'static>> = vec![Line::from(vec![
        Span::styled("filter ", theme::muted()),
        Span::styled(ui.graph_picker_query.clone(), theme::body()),
        Span::styled("▏", Style::new().fg(theme::VIOLET)),
    ])];

    if matches.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no files match — Backspace to widen",
            theme::muted(),
        )));
    }
    for (i, file) in matches.iter().enumerate().take(last).skip(first) {
        let is_sel = i == selected;
        let is_focus = *file == graph.focus;
        let marker = if is_sel { "▸ " } else { "  " };
        let mut style = theme::body();
        if is_sel {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let name = (*file)
            .chars()
            .take((w as usize).saturating_sub(6))
            .collect::<String>();
        let mut spans = vec![
            Span::styled(marker.to_string(), style.fg(theme::ACCENT)),
            Span::styled(name, style),
        ];
        // Mark the file the neighborhood is currently rooted on (the default).
        if is_focus {
            spans.push(Span::styled("  · current", theme::muted()));
        }
        lines.push(Line::from(spans));
    }

    // Pad so the legend sits on the last interior row regardless of match count.
    while lines.len() < inner_h.saturating_sub(1).max(1) {
        lines.push(Line::default());
    }
    lines.push(Line::from(Span::styled(
        " type to filter · ↑/↓ select · enter open · esc close",
        theme::muted(),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::accent())
        .title(format!(" files · {} indexed ", graph.files.len()));
    Paragraph::new(lines).block(block).render(popup, buf);
}

/// Cap on the deck composer's visible rows — it grows with the prompt up to
/// this, then scrolls (with a gutter indicator) to keep the cursor row in view.
const DECK_COMPOSER_MAX_ROWS: usize = 4;

/// The always-on composer — typing works from any tab. A multi-line textarea:
/// rows come pre-wrapped from [`crate::composer::layout`]; every row carries a
/// literal accent `>>> ` prefix (chrome, never part of the submitted text), and
/// an empty composer is a single `>>> ` line with the caret right after it.
/// Beyond [`DECK_COMPOSER_MAX_ROWS`] the box stops growing and scrolls, showing
/// a slim thumb in the right gutter while keeping the caret in view.
///
/// The caret is **steady**, not blinking: a blink carries no information the
/// reversed cell doesn't already carry, and a terminal has no
/// `prefers-reduced-motion` for a reader who needs it off.
fn render_composer(layout: &ComposerLayout, area: Rect, buf: &mut Buffer) {
    let visible = (area.height as usize).max(1);
    let total = layout.rows.len();
    let first = composer_scroll_first(layout.cursor_row, visible);

    let cursor_style = theme::accent().add_modifier(Modifier::REVERSED);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, row) in layout.rows.iter().enumerate().skip(first).take(visible) {
        // The accent `>>> ` prefix rides every row and scrolls with it —
        // exactly the four columns `PROMPT_PREFIX_W` reserves.
        let mut spans = vec![Span::styled(
            PROMPT_PREFIX,
            Style::default().fg(theme::ACCENT),
        )];
        if i == layout.cursor_row {
            let (before, under, after) = split_row_at(row, layout.cursor_col);
            let under_ch = under.map(String::from).unwrap_or_else(|| " ".into());
            spans.push(Span::styled(before, theme::body()));
            spans.push(Span::styled(under_ch, cursor_style));
            spans.push(Span::styled(after, theme::body()));
        } else {
            spans.push(Span::styled(row.clone(), theme::body()));
        }
        lines.push(Line::from(spans));
    }

    // Reserve the last column for the scroll gutter so text never collides
    // with the indicator.
    let text_area = Rect {
        width: area.width.saturating_sub(COMPOSER_GUTTER_W as u16),
        ..area
    };
    Paragraph::new(lines).render(text_area, buf);

    if total > visible {
        render_scroll_gutter(first, visible, total, area, buf);
    }
}

/// The first visible composer row for a given viewport height: the scroll
/// offset that keeps the caret's row always within the window. Split out of
/// [`render_composer`] so [`composer_cursor_position`] computes the exact
/// same windowing rather than a second copy that could drift from it.
fn composer_scroll_first(cursor_row: usize, visible: usize) -> usize {
    if cursor_row < visible {
        0
    } else {
        cursor_row + 1 - visible
    }
}

/// Absolute screen cell of the composer caret, or `None` if the composer
/// area is degenerate (nothing was drawn for it to sit in). Ratatui hides
/// the hardware cursor whenever a frame never positions it — which starves
/// CJK/IME candidate windows (they anchor to the terminal cursor, not to
/// styled cells) and gives screen readers nothing to track (#935).
fn composer_cursor_position(layout: &ComposerLayout, area: Rect) -> Option<(u16, u16)> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let visible = (area.height as usize).max(1);
    let first = composer_scroll_first(layout.cursor_row, visible);
    let row_in_view = layout.cursor_row.checked_sub(first)?;
    let y = area.y.checked_add(u16::try_from(row_in_view).ok()?)?;
    let x = area
        .x
        .checked_add(u16::try_from(PROMPT_PREFIX_W + layout.cursor_col).ok()?)?;
    Some((x, y))
}

/// A slim scrollbar in the composer's right gutter: a dim track with a violet
/// thumb sized/positioned to the visible window over `total` rows.
fn render_scroll_gutter(first: usize, visible: usize, total: usize, area: Rect, buf: &mut Buffer) {
    let h = area.height as usize;
    if h == 0 || total <= visible {
        return;
    }
    let gx = area.x + area.width.saturating_sub(1);
    // Thumb height proportional to the visible fraction (≥ 1 row).
    let thumb_h = ((visible * h) / total).max(1).min(h);
    let max_off = total.saturating_sub(visible);
    let thumb_top = (first * (h - thumb_h)).checked_div(max_off).unwrap_or(0);
    for i in 0..h {
        if let Some(cell) = buf.cell_mut((gx, area.y + i as u16)) {
            let on = i >= thumb_top && i < thumb_top + thumb_h;
            cell.set_symbol(if on { "▐" } else { "│" });
            cell.set_fg(if on { theme::VIOLET } else { theme::HAIRLINE });
        }
    }
}

// The `?` overlay — SPEC 11's key sheet and SPEC 5's metric detail. Split out
// rather than grown here: this file was a grandfathered god file at the time,
// and the metric rows #4188 asks for did not fit in its four lines of
// headroom. The split retired its baseline entry; `help.rs`'s module doc
// carries the argument.
mod help;
use help::render_help;

mod parked;

#[cfg(test)]
mod tests;
