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
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Tabs, Widget};
use unicode_width::UnicodeWidthChar;

use crate::cache_panel;
use crate::composer::{ComposerLayout, layout as composer_layout, split_row_at};
use crate::deck::{DeckTab, WorkspaceModel};
use crate::deck_ui::{DeckUi, InstalledMode, IssuesMode};
use crate::panel_guard::{guarded_band, guarded_overlay};
use crate::render::{render_slash_popup, scroll_window_start, slash_popup_area};
use crate::textline;
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

    // tab bar (bordered, exactly 3 rows) | content | trace strip (2 rows) |
    // run progress bar |
    // composer | composer footer | statline. The progress bar is always present
    // (idle collapses it to a flat track). The composer grows with its
    // soft-wrapped content up to a cap, then scrolls to keep the cursor visible;
    // its text width is the frame minus the 4-column `>>> ` prefix and the
    // 1-column scroll gutter.
    let text_w = (area.width as usize).saturating_sub(PROMPT_PREFIX_W + COMPOSER_GUTTER_W);
    let c_layout = composer_layout(&ui.composer, text_w.max(1));
    let composer_h = c_layout.rows.len().clamp(1, DECK_COMPOSER_MAX_ROWS) as u16;
    // 2 rows (labels over values), and a third only when the focused agent
    // earned a low-hit-rate diagnosis (#267). The role-pin row that used to
    // sit between them is gone: `/models` answers routing on demand, in full.
    let has_diagnosis = model
        .agents
        .get(ui.focused)
        .and_then(|a| a.cache_diagnosis(cache_panel::LOW_HIT_RATE_THRESHOLD))
        .is_some();
    let statline_h = if has_diagnosis { 3 } else { 2 };
    let bands = Layout::vertical([
        Constraint::Length(3),          // tab bar
        Constraint::Min(1),             // active view
        Constraint::Length(2),          // trace micro-summary strip (rule + line)
        Constraint::Length(1),          // run progress bar
        Constraint::Length(composer_h), // composer
        Constraint::Length(1),          // composer footer (keys + line counter)
        Constraint::Length(statline_h), // statline (label over value[, diagnosis])
    ])
    .split(area);

    let tab = ui.tab;
    guarded_band(buf, bands[0], "tab bar", |b| {
        render_tab_bar(tab, bands[0], b)
    });

    let content = bands[1];
    guarded_band(buf, content, tab.title(), |b| match tab {
        DeckTab::Session => views::session::render(model, ui, content, b),
        DeckTab::Agents => views::agents::render(model, ui, content, b),
        DeckTab::Traces => views::traces::render(model, ui, content, b),
        DeckTab::Graph => views::graph::render(model, ui, content, b),
        DeckTab::Files => views::files::render(model, ui, content, b),
        DeckTab::Skills => views::skills::render(model, ui, content, b),
        DeckTab::Mcp => views::mcp::render(model, ui, content, b),
        DeckTab::Issues => views::issues::render(model, ui, content, b),
        DeckTab::Settings => views::settings::render(model, ui, content, b),
    });

    guarded_band(buf, bands[2], "traces", |b| {
        render_trace_strip(model, bands[2], b)
    });
    guarded_band(buf, bands[3], "progress", |b| {
        crate::progress::render(model, ui, bands[3], b)
    });
    guarded_band(buf, bands[4], "composer", |b| {
        render_composer(&c_layout, bands[4], b)
    });
    guarded_band(buf, bands[5], "footer", |b| {
        render_composer_footer(model, ui, &c_layout, bands[5], b)
    });
    guarded_band(buf, bands[6], "statline", |b| {
        crate::statline::render(model, ui, bands[6], b)
    });
    let composer_cursor = composer_cursor_position(&c_layout, bands[4]);

    // Floating popups sit above the chrome: the slash menu anchors to the
    // composer; the queue editor centers over the content.
    let slash = ui.composer.slash_menu(&ui.slash_commands);
    let slash_open = slash.as_ref().is_some_and(|m| !m.is_empty());
    if let Some(menu) = slash.filter(|m| !m.is_empty()) {
        let selected = ui.slash_selected.min(menu.matches.len().saturating_sub(1));
        let popup = slash_popup_area(area, bands[4], menu.matches.len());
        // Live values in descriptions, read from the model at render time —
        // never cached in `DeckUi` (D3).
        let live = slash_live_hints(model, ui);
        guarded_band(buf, popup, "slash menu", |b| {
            render_slash_popup(&menu, selected, &live, popup, b)
        });
    }
    if ui.queue_open {
        guarded_overlay(buf, area, "queue", |b| {
            views::queue_popup::render(model, ui, area, b)
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
            render_sessions_overlay(model, ui, area, b)
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
    // The plan-review dialog: modal while the focused agent's scope gate is
    // pending and unanswered — it is the surface that halts the session.
    // Below the cards on purpose: raising `/plan` over it is how a reviewer
    // reads the envelope detail before deciding (Esc lowers the card and the
    // dialog is back on top).
    if let Some(proposal) = views::scope_dialog::pending(model, ui) {
        guarded_overlay(buf, area, "plan review", |b| {
            views::scope_dialog::render(proposal, ui.scope_input.as_ref(), area, b)
        });
    }
    // The floating cards (`/plan` · `/models` · `/budget`): at most one is up
    // (`CardState::raise` lowers the rest); help and the startup notice still
    // win the top of the stack below.
    if let Some(card) = ui.cards.open {
        use crate::deck_ui::cards::Card;
        match card {
            Card::Plan => guarded_overlay(buf, area, "plan card", |b| {
                views::plan_card::render(model, ui, area, b)
            }),
            Card::Models => guarded_overlay(buf, area, "models card", |b| {
                views::models_card::render(model, ui, area, b)
            }),
            Card::Budget => guarded_overlay(buf, area, "budget card", |b| {
                views::budget_card::render(model, ui, area, b)
            }),
        }
    }
    // (The former ENGINE overlay is gone: the engine panel is the full-width
    // body of the SETTINGS tab — see `views::settings::render`.)

    // Startup system notifications: a transient dialog over the deck, drawn
    // last but one so help — which the user asked for — still wins the top.
    // It is a no-op once dismissed or expired — asked here rather than left to
    // `notice::render`'s own early return, so the guard's scratch copy is paid
    // for only on the frames a notice is actually up.
    if ui.notice.is_visible() {
        guarded_overlay(buf, area, "notice", |b| notice::render(&ui.notice, area, b));
    }

    if ui.help_open {
        guarded_overlay(buf, area, "help", |b| render_help(ui, area, b));
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
        || ui.queue_open
        || ui.graph_picker_open
        || ui.sessions_open
        || ui.inbox_open
        || ui.context_open
        || ui.inspect_open
        // The floating cards are modal while up (their handler owns every
        // key ahead of the composer — see `deck_ui::cards`).
        || ui.cards.is_open()
        || slash_open
        // The routing card holds the user's words and owns every key until
        // they say where they go — it is checked first in `handle_deck_key`.
        || ui.pending_dispatch.is_some()
        // The plan-review dialog answers on a single keypress — the caret
        // must not sit in the composer while it is up (`gates.rs`).
        || views::scope_dialog::pending(model, ui).is_some()
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

/// The deck's tab navigation bar.
///
/// Built from ratatui's own [`Tabs`] inside a full-border [`Block`]: the labels
/// occupy the single inner row, which is exactly the 3 rows the deck layout
/// reserves for the bar. Overflow clips on the right (at narrow widths the
/// rightmost tabs fall off) — the selected tab is always drawn because the
/// layout never scrolls, matching the previous bar's behaviour at deck widths.
fn render_tab_bar(tab: DeckTab, area: Rect, buf: &mut Buffer) {
    let labels: Vec<&str> = DeckTab::ALL.iter().map(|t| t.title()).collect();
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule());
    // The corner wordmark: the brand riding the top border row, which holds
    // nothing but rule characters — zero rows of chrome spent. Dropped on
    // narrow frames rather than crowding the border.
    if area.width >= 44 {
        block = block.title_top(
            Line::from(vec![
                Span::styled("✦ ", Style::new().fg(theme::GOLD)),
                Span::styled("stella ", theme::muted()),
            ])
            .right_aligned(),
        );
    }
    let tabs = Tabs::new(labels)
        .select(tab.index())
        .style(theme::muted())
        .highlight_style(theme::accent())
        .divider(symbols::line::VERTICAL)
        .block(block);
    Widget::render(tabs, area, buf);
}

// The queue editor popup lives in `views::queue_popup` (split out beside the
// other popup renderers under the god-file rule).

/// The SESSIONS overlay (empty-prompt `←`, `/sessions`): every stella
/// session on this machine from the cross-process registry, grouped by
/// status in [`crate::envelope::SessionPhase::ALL`] order, each with its
/// human title and a summary of the work involved. Selection walks the
/// flattened rows ([`crate::deck_ui::grouped_session_rows`]).
fn render_sessions_overlay(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    let w = area.width.saturating_sub(6).min(110);
    let h = area.height.saturating_sub(4).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    Clear.render(popup, buf);

    let rows = crate::deck_ui::grouped_session_rows(ui);
    let selected = ui.sessions_sel.min(rows.len().saturating_sub(1));
    let mut lines: Vec<Line<'static>> = Vec::new();

    if rows.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "  no stella sessions registered yet",
            theme::muted(),
        )));
    }

    // Two lines per session + one heading per non-empty group; window on the
    // *selected session* so long lists keep it in view.
    let visible_sessions = ((h as usize).saturating_sub(4) / 3).max(1);
    let start = selected
        .saturating_sub(visible_sessions.saturating_sub(1) / 2)
        .min(rows.len().saturating_sub(visible_sessions));

    let mut flat_idx = 0usize;
    let mut emitted = 0usize;
    for phase in crate::envelope::SessionPhase::ALL {
        let group: Vec<_> = rows.iter().filter(|s| s.phase == phase).collect();
        if group.is_empty() {
            continue;
        }
        let mut heading_emitted = false;
        for session in group {
            let in_window = flat_idx >= start && emitted < visible_sessions;
            if in_window {
                if !heading_emitted {
                    lines.push(Line::from(Span::styled(
                        format!("  {} ({})", phase.label().to_uppercase(), {
                            rows.iter().filter(|s| s.phase == phase).count()
                        }),
                        theme::accent().add_modifier(Modifier::BOLD),
                    )));
                    heading_emitted = true;
                }
                let is_sel = flat_idx == selected;
                let marker = if is_sel { "▸ " } else { "  " };
                let dot = Span::styled("● ", Style::default().fg(phase_color(phase)));
                let mut title_style = Style::default().fg(theme::INK);
                if is_sel {
                    title_style = title_style
                        .bg(theme::SELECT_BG)
                        .add_modifier(Modifier::BOLD);
                }
                let mine = if session.mine { "  (this session)" } else { "" };
                // The ⏎ affordance rides the selected row, right where the
                // eye already is — every resumable row also carries a subtle
                // ↩ so the list is scannable for "where can I go back in".
                let tag = if session.resumable && !session.mine {
                    if is_sel { "  ↩ ⏎ resume" } else { "  ↩" }
                } else {
                    ""
                };
                let title: String = session
                    .title
                    .chars()
                    .take((w as usize).saturating_sub(24 + mine.len() + tag.chars().count()))
                    .collect();
                lines.push(Line::from(vec![
                    Span::raw(marker),
                    dot,
                    Span::styled(title, title_style),
                    Span::styled(mine.to_string(), theme::muted()),
                    Span::styled(tag.to_string(), theme::accent()),
                ]));
                let summary = if session.summary.is_empty() {
                    "(no work recorded yet)".to_string()
                } else {
                    session.summary.clone()
                };
                let detail = format!(
                    "      {} — {} · {}",
                    truncate_chars(&summary, (w as usize).saturating_sub(40)),
                    session.workspace,
                    fmt_age(model.now_ms.saturating_sub(session.updated_ms)),
                );
                lines.push(Line::from(Span::styled(
                    truncate_chars(&detail, (w as usize).saturating_sub(4)),
                    theme::muted(),
                )));
                emitted += 1;
            }
            flat_idx += 1;
        }
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " ↑/↓ select · ↵ resume/open · a archive · x delete · r refresh · esc/← close",
        theme::muted(),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::accent())
        .title(format!(" stella sessions · {} ", rows.len()));
    Paragraph::new(lines).block(block).render(popup, buf);
}

/// The color of a session-phase dot (ember palette; no pink/purple).
fn phase_color(phase: crate::envelope::SessionPhase) -> ratatui::style::Color {
    use crate::envelope::SessionPhase;
    match phase {
        SessionPhase::InProgress => theme::SUCCESS_BRIGHT,
        SessionPhase::NeedsInput => theme::WARNING_BRIGHT,
        SessionPhase::Paused => theme::ACCENT,
        SessionPhase::Cancelled => theme::TEXT_TERTIARY,
        // The same calm tone as `Cancelled`: both are deliberate endings,
        // and the whole point of the variant is not to paint them red.
        SessionPhase::Stopped => theme::TEXT_TERTIARY,
        SessionPhase::Complete => theme::SUCCESS,
        SessionPhase::Archived => theme::TEXT_TERTIARY,
        SessionPhase::Error => theme::DANGER_BRIGHT,
    }
}

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
        let heading = crate::views::mcp::compact_heading(server);
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
    // warmth countdown, engine and pipeline state. Diagnostics, not
    // glanceables — this overlay is where a reader who wants them looks.
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
        Span::styled("  ·  pipeline ", dim),
        if model.pipeline {
            Span::styled("on", Style::default().fg(theme::SUCCESS_BRIGHT))
        } else {
            Span::styled("off", dim)
        },
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
        if let Some((text, integrity)) = view.digest_mismatch_line() {
            let tone = if integrity {
                theme::DANGER
            } else {
                theme::WARN
            };
            lines.push(Line::from(Span::styled(text, Style::default().fg(tone))));
        }
        if view.verified {
            lines.push(Line::from(Span::styled(
                "  verified · every journal-resolved block re-hashed to its recorded digest",
                Style::default().fg(theme::SUCCESS),
            )));
        }
        for (index, message) in view.messages.iter().enumerate() {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                format!("  ─── [{index}] {} ───", message.role),
                theme::accent().add_modifier(Modifier::BOLD),
            )));
            // Wrap by hand: Paragraph's own wrap would fight the scroll offset
            // (rows would no longer equal lines, so the clamp below would lie).
            for raw in message.content.lines() {
                for chunk in wrap_chars(raw, (w as usize).saturating_sub(6)) {
                    lines.push(Line::from(Span::styled(
                        format!("    {chunk}"),
                        Style::default().fg(theme::INK),
                    )));
                }
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

/// The deck-wide transcript micro-summary strip: a hairline rule with, under
/// it, one dimmed line summarizing the NEWEST entry of the cross-agent trace
/// ([`WorkspaceModel::trace`]) — a glanceable "what just happened" on every
/// tab, refreshed naturally every frame. Sits directly above the composer
/// chrome (two rows: rule + summary).
fn render_trace_strip(model: &WorkspaceModel, area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let rule: String = "─".repeat(area.width as usize);
    Paragraph::new(Line::from(Span::styled(rule, theme::rule())))
        .render(Rect { height: 1, ..area }, buf);
    if area.height < 2 {
        return;
    }
    // A fixed left anchor so the live summary reads as a labeled "latest
    // activity" status line rather than words floating loose under the
    // transcript box — the churn of streaming trace rows is what made the
    // strip look like a leak. The anchor stays put; only the value moves.
    let anchor = Span::styled(" ▸ ", Style::default().fg(theme::TEXT_TERTIARY));
    let line = match model.trace.rows.back() {
        Some(row) => Line::from(vec![
            anchor,
            // The kind keeps its colour (scannable); the summary stays dim so
            // the strip never competes with the transcript or the prompt.
            Span::styled(
                row.kind.label(),
                Style::default().fg(theme::trace_kind_color(row.kind)),
            ),
            Span::styled("  ", Style::default()),
            Span::styled(
                truncate_chars(&row.summary, (area.width as usize).saturating_sub(12)),
                Style::default().fg(theme::TEXT_TERTIARY),
            ),
        ]),
        None => Line::from(vec![
            anchor,
            Span::styled("no activity yet", Style::default().fg(theme::TEXT_TERTIARY)),
        ]),
    };
    Paragraph::new(line).render(
        Rect {
            y: area.y + 1,
            height: 1,
            ..area
        },
        buf,
    );
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

/// Total display width of a span run, in terminal columns.
fn span_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| display_width(&s.content)).sum()
}

/// Render `spans` right-aligned at the end of `area` without clearing the rest
/// of the row — draws only its own sub-rect, so a left-aligned line drawn first
/// survives everywhere it doesn't overlap.
fn render_right(spans: Vec<Span<'static>>, area: Rect, buf: &mut Buffer) {
    let w = span_width(&spans).min(area.width as usize) as u16;
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w),
        y: area.y,
        width: w,
        height: 1,
    };
    Paragraph::new(Line::from(spans)).render(rect, buf);
}

/// The quiet keybind + line-counter row directly under the composer and above
/// the statline. Keybind glyphs are violet; the right end carries the
/// live line counter and the queue status.
fn render_composer_footer(
    model: &WorkspaceModel,
    ui: &DeckUi,
    _layout: &ComposerLayout,
    area: Rect,
    buf: &mut Buffer,
) {
    if area.height == 0 {
        return;
    }
    let key = Style::default().fg(theme::VIOLET);
    let dim = Style::default().fg(theme::TEXT_TERTIARY);
    let sep = Style::default().fg(theme::HAIRLINE);

    // Right: live logical-line counter · queue status. Built first so its width
    // is known and the left affordances can be clipped to what remains.
    let n_lines = ui.composer.buffer().split('\n').count().max(1);
    let counter = format!("{n_lines} line{}", if n_lines == 1 { "" } else { "s" });
    let pending = model.queue.pending();
    let (q_text, q_style) = if pending > 0 && ui.dispatch_held {
        (
            format!("{pending} held"),
            Style::default().fg(theme::DANGER),
        )
    } else if pending > 0 {
        (
            format!("{pending} queued"),
            Style::default().fg(theme::WARNING_BRIGHT),
        )
    } else {
        ("queue empty".to_string(), dim)
    };
    // Live turn cost, pinned nearest the composer's left edge of the right
    // cluster — the readout that replaced the `◇ spend` rows the transcript
    // used to print once per model call.
    //
    // A gauge, not an event: it updates in place as ticks land and settles on
    // the turn's real total when `Complete` arrives, at which point the
    // transcript prints the same figure in the same green (`✓ cost`) as the
    // turn's permanent receipt. Four decimals, because the whole value of a
    // live cost readout is watching it move, and at two decimals most turns
    // never leave `$0.00`.
    let turn_cost = model
        .agents
        .get(ui.focused)
        .map_or(0.0, |a| a.model.hud.turn_spent_usd());
    let right = vec![
        Span::styled("turn ", dim),
        Span::styled(
            textline::fmt_cost(turn_cost),
            Style::default()
                .fg(theme::SUCCESS_BRIGHT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", sep),
        Span::styled(counter, dim),
        Span::styled("  ·  ", sep),
        Span::styled(q_text, q_style),
        Span::raw(" "),
    ];
    let right_w = span_width(&right) as u16;

    // Advertise the newline chord the terminal can actually report: `⌘⏎`/`⌃⏎`
    // where the kitty protocol is live, else the universally-safe `⌥⏎`. Drop
    // the lower-value affordances first on a narrow row so nothing collides
    // with the counter.
    let newline = if ui.enter_submits { "⌥⏎" } else { "⌘⏎" };
    let mut left = vec![
        Span::raw(" "),
        Span::styled(newline, key),
        Span::styled(" new line", dim),
        Span::styled("  ·  ", sep),
        Span::styled("⏎", key),
        Span::styled(" queue (never blocks)", dim),
    ];
    // `>` steers the running turn — the highest-value affordance on this row,
    // and until now advertised nowhere at all. Shown only while the focused
    // agent is actually running, because that is the only time it does
    // anything: a `>` typed at an idle agent is just the next prompt. It also
    // takes priority over `!`/`/` in the width budget below, since it is the
    // one thing a user watching a turn go the wrong way needs to know.
    if model
        .agents
        .get(ui.focused)
        .is_some_and(|a| a.status == crate::AgentStatus::Running)
    {
        left.push(Span::styled("  ·  ", sep));
        left.push(Span::styled(">", key));
        left.push(Span::styled(" steer this turn", dim));
    }
    let extras = [("!", " shell"), ("/", " commands")];
    let left_budget = (area.width.saturating_sub(right_w + 1)) as usize;
    for (glyph, word) in extras {
        let add = 5 + glyph.chars().count() + word.chars().count(); // "  ·  " + glyph + word
        if span_width(&left) + add <= left_budget {
            left.push(Span::styled("  ·  ", sep));
            left.push(Span::styled(glyph, key));
            left.push(Span::styled(word, dim));
        }
    }
    let left_area = Rect {
        width: area.width.saturating_sub(right_w),
        ..area
    };
    Paragraph::new(Line::from(left)).render(left_area, buf);
    render_right(right, area, buf);
}

/// One aligned `key → description` row of the help overlay. The key column is
/// padded to a fixed width so the descriptions line up into a scannable
/// second column.
fn help_row(key: &str, desc: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {key:<13} "), theme::accent()),
        Span::styled(desc.to_string(), theme::body()),
    ])
}

/// The shortcuts specific to one deck tab, as `(key, description)` pairs.
/// Keyed off [`DeckTab`] so the overlay only ever shows keys that work where
/// the user actually is — the per-tab handlers in `deck_ui` are the behavior
/// these rows must mirror.
fn tab_shortcuts(tab: DeckTab) -> &'static [(&'static str, &'static str)] {
    match tab {
        DeckTab::Session => &[
            ("↑ ↓", "select a message · esc clears the selection"),
            ("⇞ ⇟", "scroll the transcript"),
            ("⌘[ / ⌘]", "jump to transcript start / end (⌃ works too)"),
            ("ctrl-o", "expand/collapse the selected message (none: all)"),
            ("ctrl-r", "expand/collapse all thinking"),
            (
                "ctrl-f",
                "find in the transcript — ⏎ next · ctrl-p previous",
            ),
            ("ctrl-n / ctrl-p", "jump to the next / previous failure"),
            ("ctrl-z", "fold the selected turn to one line (none: all)"),
            ("↑", "with prompts queued: open the queue editor"),
            ("←", "SESSIONS overlay — every session on this machine"),
            ("→", "CONTEXT overlay — active skills + MCP servers"),
            ("ctrl-g", "INSPECT — the context sent on any recorded call"),
            (
                "a / t / x",
                "plan review: approve · trim · abort — one keypress decides",
            ),
            (
                "r",
                "plan review: refine — type what to change, ⏎ re-plans from it",
            ),
            (
                "esc",
                "plan review: abort (from the refine input: back out)",
            ),
        ],
        DeckTab::Agents => &[
            ("← →", "switch panes — executions / installed"),
            ("↑ ↓", "select an agent"),
            ("⏎", "open the selected agent's session"),
            ("s / p / r", "stop · pause · restart the selected agent"),
            ("⏎", "installed pane: edit · v versions · r reload"),
            ("n", "new agent — drafted by the LLM"),
        ],
        DeckTab::Traces => &[
            ("↑ ↓ ⇞ ⇟", "scroll the event log"),
            ("f", "cycle the per-agent filter"),
        ],
        DeckTab::Graph => &[
            ("← → ↑ ↓", "walk the neighborhood"),
            ("/ or ⏎", "file picker — re-root on any indexed file"),
        ],
        DeckTab::Files => &[("↑ ↓", "select a file"), ("⏎", "open / close the diff")],
        DeckTab::Skills => &[
            ("← →", "switch panes"),
            ("↑ ↓", "select a skill"),
            ("space", "enable / disable"),
            ("e", "edit the selected skill"),
            ("p", "pin / unpin"),
            ("n", "new skill — drafted by the LLM"),
            ("ctrl-o", "preview"),
            ("ctrl-x ×2", "delete (press twice to confirm)"),
            ("type", "search skills"),
        ],
        DeckTab::Mcp => &[
            ("↑ ↓", "select a server"),
            ("space / e", "enable / disable"),
            ("a", "authenticate (env credentials)"),
            ("o", "OAuth login (http servers)"),
            ("s", "search the registry"),
            ("x", "remove the server"),
            ("r", "refresh"),
        ],
        DeckTab::Issues => &[
            ("↑ ↓", "select an issue"),
            ("r", "refresh the list"),
            ("/", "search the tracker"),
            ("n", "new issue — tab cycles fields · ctrl-s creates"),
            ("c", "comment on the selected issue"),
            ("s", "set the selected issue's status"),
            ("w", "start work on the selected issue"),
        ],
        DeckTab::Settings => &[
            ("← →", "switch panes — agents / tools"),
            ("e", "edit the pane you are on"),
            ("t", "jump to the tool switches"),
            (
                "tab",
                "in the editor: switch agent — global / default / worker / …",
            ),
            ("⏎", "in the editor: edit the selected row / pick a model"),
            ("space", "in the editor: toggle the selected row"),
            ("x", "in the editor: clear the selected row"),
            ("s / S", "in the editor: save to user / project settings"),
            ("r", "in the editor: reload from disk"),
            ("esc", "in the editor: hand the keyboard back to the tab"),
        ],
    }
}

/// Deck-wide shortcuts that work on every tab.
const GLOBAL_SHORTCUTS: &[(&str, &str)] = &[
    ("tab / ⇧tab", "switch tabs"),
    // Enter semantics are the inverse of the old chord-to-submit mapping (see
    // `composer::classify_enter`): a bare ⏎ dispatches, a *modified* ⏎ breaks
    // the line. The composer footer advertises the same pair — these rows must
    // not drift from it.
    // The parenthetical is load-bearing: the unqualified promise ("runs as its
    // own agent") is what a reviewer read while a scope card was waiting, and
    // the sidecar it describes is exactly what swallowed their answer.
    (
        "⏎",
        "queue the prompt — mid-turn it runs as its own agent (unless a card waits)",
    ),
    ("⌘⏎ / ⌃⏎ / ⌥⏎", "insert a line break in the prompt"),
    ("!cmd", "run a shell command NOW (skips the queue)"),
    ("/", "slash commands — ↑↓ pick · tab completes · ⏎ runs"),
    ("ctrl-v", "paste — a copied image is attached to the prompt"),
    ("ctrl-t", "open the queue editor"),
    (
        "ctrl-s",
        "STATE — the approved scope's steps, the task board, the proof rail",
    ),
    (
        ">text",
        "steer the running turn — lands at the next step boundary",
    ),
    (
        "esc",
        "soft-stop at the next step boundary — completed work kept",
    ),
    (
        "esc esc",
        "cancel NOW & hold — nothing runs until your next prompt",
    ),
    ("ctrl-c", "quit stella"),
];

/// The help overlay: the active tab's keys first, then the deck-wide keys —
/// one shortcut per line, key column aligned. Context-aware on purpose: only
/// shortcuts that work on the tab the user is looking at are shown, so the
/// overlay stays short enough to read at a glance. Opened by `?` (empty
/// composer) or `/help`; scrolls with ↑/↓/⇞/⇟/Home/End on a short terminal;
/// closes with esc/`q`/`?`.
fn render_help(ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        format!("  {} tab", ui.tab.title()),
        theme::heading(),
    )));
    for (key, desc) in tab_shortcuts(ui.tab) {
        lines.push(help_row(key, desc));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled("  everywhere", theme::heading())));
    for (key, desc) in GLOBAL_SHORTCUTS {
        lines.push(help_row(key, desc));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "  letter & arrow hotkeys apply while the prompt box is empty",
        theme::muted(),
    )));

    // Size the panel to its content, capped to the frame.
    let w = area.width.min(68);
    let h = area.height.min(lines.len() as u16 + 2);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    Clear.render(popup, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::accent())
        .title(format!(" help — {} · esc close ", ui.tab.title()));
    let inner = block.inner(popup);
    block.render(popup, buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let total = lines.len();
    let height = inner.height as usize;
    // Record viewport metrics for the pure key handler (`handle_help_key`) —
    // when the panel is clipped, ↑/↓/⇞/⇟/Home/End scroll it.
    ui.metrics.help_total = total;
    ui.metrics.help_height = height;
    let window = ui.help_scroll.window(total, height);
    Paragraph::new(lines)
        .scroll((window.start as u16, 0))
        .render(inner, buf);
}

#[cfg(test)]
mod tests;
