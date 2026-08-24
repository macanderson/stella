// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The SKILLS tab's dialogs: the scope picker, the LLM-assisted create flow
//! (description → creating → failed), the version pin picker, the `SKILL.md`
//! editor, and the `ctrl+o` markdown preview.
//!
//! Each one floats over the tab's two source boxes and frames itself, because
//! it is drawn above the content area rather than inside it — a dialog with no
//! border of its own would read as rows the tab had appended. The keys ride the
//! bottom rule the way [`crate::v2::sessions`] and [`crate::v2::subagents`] put
//! theirs, so the body carries only what the dialog is about.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget, Wrap};
use stella_tui_theme::token;

use crate::deck_ui::{DeckUi, ScopeAction, SkillPrompt};
use crate::syntax::{self, HighlightSpans as _};

use super::{centered_row, truncate};

/// Widest dialog, in columns. The pin picker and the scope picker are lists of
/// short labels; anything wider reads as a pane rather than a question.
const DIALOG_W: u16 = 60;

/// Draw the open prompt centered over `area`. `now_ms` is the deck clock — the
/// creating dialog's spinner is a pure function of it.
pub fn render_prompt(ui: &DeckUi, now_ms: u64, area: Rect, buf: &mut Buffer) {
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    match &ui.skills.prompt {
        Some(SkillPrompt::Scope { action, user }) => {
            let verb = match action {
                ScopeAction::Install { id } => format!("Install {id}"),
                ScopeAction::Create { .. } => "Create the new skill".to_string(),
            };
            let choose = |label: &str, hint: &str, selected: bool| {
                let marker = if selected { "▸ " } else { "  " };
                let style = if selected {
                    Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD)
                } else {
                    text
                };
                Line::from(vec![
                    Span::styled(marker, Style::new().fg(token::GOLD)),
                    Span::styled(label.to_string(), style),
                    Span::styled(format!("  {hint}"), muted),
                ])
            };
            let lines = vec![
                Line::from(Span::styled(verb, text)),
                Line::from(Span::styled("Where should it live?", muted)),
                Line::default(),
                choose(
                    "[p] Project",
                    ".stella/skills — travels with the repo",
                    !*user,
                ),
                choose("[u] User", "~/.stella/skills — global to you", *user),
                Line::default(),
            ];
            dialog(
                "install scope",
                "←/→ or p/u choose · ⏎ confirm · esc cancel",
                lines,
                area,
                buf,
            );
        }
        Some(SkillPrompt::CreateDescription { buffer }) => {
            let lines = vec![
                Line::from(Span::styled(
                    "Describe the skill you want (the agent will search the",
                    muted,
                )),
                Line::from(Span::styled(
                    "registry, rank matches, and assemble one skill):",
                    muted,
                )),
                Line::default(),
                Line::from(vec![
                    Span::styled("> ", Style::new().fg(token::GOLD)),
                    Span::styled(buffer.clone(), text),
                    Span::styled("▌", Style::new().fg(token::GOLD)),
                ]),
                Line::default(),
            ];
            dialog(
                "new skill (LLM-assisted)",
                "⏎ continue · esc cancel",
                lines,
                area,
                buf,
            );
        }
        Some(SkillPrompt::Creating { description, scope }) => {
            let spinner = crate::views::spinner_glyph(now_ms, ui.no_anim);
            let lines = vec![
                Line::from(vec![
                    Span::styled(
                        format!("{spinner} "),
                        Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("Creating the skill ({} scope)…", scope.label()),
                        text,
                    ),
                ]),
                Line::default(),
                Line::from(Span::styled(
                    format!("“{}”", truncate(description, 70)),
                    muted,
                )),
                Line::default(),
                Line::from(Span::styled(
                    "the agent searches the registry, ranks matches, and",
                    muted,
                )),
                Line::from(Span::styled(
                    "assembles one skill — it appears here when done",
                    muted,
                )),
                Line::default(),
            ];
            dialog(
                "creating skill…",
                "esc hides (creation continues)",
                lines,
                area,
                buf,
            );
        }
        Some(SkillPrompt::CreateFailed { error }) => {
            let lines = vec![
                Line::from(vec![
                    Span::styled(
                        "✖ ",
                        Style::new().fg(token::RED).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("The skill was not created", text),
                ]),
                Line::default(),
                Line::from(Span::styled(error.clone(), Style::new().fg(token::RED))),
                Line::default(),
            ];
            dialog("skill creation failed", "esc / ⏎ close", lines, area, buf);
        }
        Some(SkillPrompt::Pin {
            name, latest, sel, ..
        }) => {
            let mut lines = vec![
                Line::from(Span::styled(format!("Pin a version of {name}:"), text)),
                Line::default(),
            ];
            for v in 1..=*latest {
                let selected = v == *sel;
                let marker = if selected { "▸ " } else { "  " };
                let tag = if v == *latest { "  (latest)" } else { "" };
                let style = if selected {
                    Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD)
                } else {
                    text
                };
                lines.push(Line::from(vec![
                    Span::styled(marker, Style::new().fg(token::GOLD)),
                    Span::styled(format!("v{v}{tag}"), style),
                ]));
            }
            lines.push(Line::default());
            dialog(
                "pin version",
                "↑/↓ choose · ⏎ pin · esc cancel",
                lines,
                area,
                buf,
            );
        }
        Some(SkillPrompt::Edit { name, buffer, .. }) => render_edit(name, buffer, area, buf),
        None => {}
    }
}

/// The `SKILL.md` editor: the buffer with a block caret on its last line,
/// tail-scrolled so what you just typed stays visible.
///
/// The buffer is markdown source, so it highlights as markdown — the
/// frontmatter keys, headings and fenced examples that make the file navigable
/// while it is edited. Every line feeds the highlighter (fence / frontmatter
/// state spans the whole buffer); only the tail renders.
fn render_edit(name: &str, buffer: &str, area: Rect, buf: &mut Buffer) {
    let w = area.width.saturating_sub(6).clamp(20, 88);
    let h = area.height.saturating_sub(2).clamp(6, 20);
    let rect = centered(area, w, h);
    Clear.render(rect, buf);
    let block = framed(
        format!("edit {name}"),
        "ctrl+s save (new version) · esc cancel",
    );
    let inner = block.inner(rect);
    block.render(rect, buf);
    if inner.height == 0 {
        return;
    }

    let text_lines: Vec<&str> = buffer.split('\n').collect();
    let mut hl = syntax::Highlighter::new(Some(syntax::Lang::Markdown));
    let styled: Vec<Vec<Span<'static>>> = text_lines
        .iter()
        .map(|l| hl.spans(l, Style::new().fg(token::TEXT)))
        .collect();
    let visible = inner.height as usize;
    let start = styled.len().saturating_sub(visible);
    let last = styled.len() - 1;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, mut spans) in styled.into_iter().enumerate().skip(start) {
        if i == last {
            spans.push(Span::styled("▌", Style::new().fg(token::GOLD)));
        }
        lines.push(Line::from(spans));
    }
    Paragraph::new(lines).render(inner, buf);
}

/// The `ctrl+o` markdown preview: the skill's `SKILL.md` rendered through
/// Stella's own markdown renderer (the one the transcript uses, so the preview
/// stays inside the deck's palette) and scrolled vertically. A `None` body is
/// the loading state; the scroll offset is clamped to the content here, because
/// the key handler only ever increments it.
pub fn render_preview(ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    let Some(preview) = ui.skills.preview.as_mut() else {
        return;
    };
    let w = area.width.saturating_sub(4).clamp(24, 100);
    let h = area.height.saturating_sub(2).clamp(6, 32);
    let rect = centered(area, w, h);
    Clear.render(rect, buf);
    let title = truncate(&preview.title, (w as usize).saturating_sub(20).max(8));
    let block = framed(title, "↑/↓ scroll · esc close");
    let inner = block.inner(rect);
    block.render(rect, buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    // A muted sub-line (url / scope), then the scrollable body below it.
    let bands = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
    if !preview.subtitle.is_empty() {
        Paragraph::new(Line::from(Span::styled(
            truncate(&preview.subtitle, inner.width as usize),
            Style::new().fg(token::MUTED),
        )))
        .render(bands[0], buf);
    }
    let body_area = bands[1];

    match preview.body.clone() {
        None => {
            Paragraph::new("fetching SKILL.md…")
                .style(Style::new().fg(token::MUTED))
                .alignment(Alignment::Center)
                .render(centered_row(body_area), buf);
        }
        Some(body) => {
            let text = Text::from(crate::markdown::render(&body));
            let content_h = text.height();
            let max_scroll = content_h.saturating_sub(body_area.height as usize) as u16;
            let scroll = preview.scroll.min(max_scroll);
            preview.scroll = scroll;
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0))
                .render(body_area, buf);
        }
    }
}

/// A centered dialog with `title` on the top rule and `keys` on the bottom one.
fn dialog(title: &str, keys: &str, lines: Vec<Line<'static>>, area: Rect, buf: &mut Buffer) {
    let w = area.width.min(DIALOG_W);
    let h = ((lines.len() + 2) as u16).min(area.height);
    let rect = centered(area, w, h);
    Clear.render(rect, buf);
    Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .block(framed(title, keys))
        .render(rect, buf);
}

/// The dialog frame every one of these shares: a rounded rule, the name on the
/// top edge, the keys that dismiss it on the bottom edge.
fn framed(title: impl Into<String>, keys: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(token::BORDER))
        .title(Line::from(Span::styled(
            format!(" {} ", title.into()),
            Style::new().fg(token::TEXT),
        )))
        .title_bottom(
            Line::from(Span::styled(
                format!(" {keys} "),
                Style::new().fg(token::DIM),
            ))
            .right_aligned(),
        )
}

/// A `w`×`h` rect centered in `area`.
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}
