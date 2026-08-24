// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The AGENTS tab — the agents installed on disk at the user
//! (`~/.stella/agents`) and project (`.stella/agents`) levels, plus the modal
//! sub-views that author them (the definition editor, the create-from-prompt
//! flow, and the version picker).
//!
//! ```text
//!  agents · 2 installed · lead is reviewer
//!
//!    agent            scope    ver  description                    toolbelt
//!  ✦ reviewer         project  v2   reviews a diff                  read_file, search
//!  ▸ release-captain  user     v1   cuts a release                  all tools
//!  ↵ edit · a assume · n new · x x delete · v versions · r reload
//! ```
//!
//! Every mode wears the same three bands — a header naming what you are
//! looking at, the body, and a key row — so switching into the editor or the
//! version picker moves the content and nothing else. That grammar is why
//! none of them draws a box: [`super::frame`] already carved the content area
//! out of the deck, and a border around all of it would re-spend the rows
//! SPEC 5 reclaimed while saying only "this tab has edges".
//!
//! The list content comes verbatim from the driver's
//! [`crate::envelope::Inbound::AgentsList`] snapshot held on
//! [`crate::deck_ui::InstalledPanel`] (no shadow state).

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, Widget, Wrap};
use stella_tui_theme::{glyph, token};

use crate::composer;
use crate::deck_ui::{DeckUi, InstalledMode, InstalledPanel};
use crate::envelope::InstalledAgentEntry;
use crate::syntax::{self, HighlightSpans as _};

/// Draw the tab. `now_ms` is the deck clock, which is all this tab takes off
/// the model — it has no model-derived state and no key handler of its own
/// (`deck_ui.rs` routes its keys directly).
pub fn render(ui: &mut DeckUi, now_ms: u64, area: Rect, buf: &mut Buffer) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let accessible = ui.accessible;
    let no_anim = ui.no_anim;
    match ui.installed.mode {
        InstalledMode::Browse => render_list(&ui.installed, accessible, area, buf),
        InstalledMode::Edit => render_editor(&ui.installed, area, buf),
        InstalledMode::CreateDescribe => render_create_describe(&ui.installed, area, buf),
        InstalledMode::CreateScope => render_create_scope(&ui.installed, area, buf),
        InstalledMode::Creating => render_creating(&ui.installed, now_ms, no_anim, area, buf),
        InstalledMode::CreateDone => render_create_done(&mut ui.installed, area, buf),
        InstalledMode::PickVersion => render_version_picker(&ui.installed, area, buf),
    }
}

/// The toolbelt cell text: the granted tools - or 'all tools'
/// when the definition doesn't restrict them.
#[must_use]
pub fn toolbelt_label(tools: &Option<Vec<String>>) -> String {
    match tools {
        None => "all tools".to_string(),
        Some(list) => list.join(", "),
    }
}

/// The tab's bands: header, a row of air, the body, the key row. Returned in
/// that order; a frame too short to hold four rows gets zero-height bands,
/// which every drawer below guards on.
fn bands(area: Rect) -> (Rect, Rect, Rect) {
    let split = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);
    (split[0], split[2], split[3])
}

/// The header row: what this mode is, then its context, in the tab row's own
/// vocabulary rather than a border title.
fn render_head(spans: Vec<Span<'static>>, area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let mut row = vec![Span::raw(" ")];
    row.extend(spans);
    Paragraph::new(Line::from(row)).render(Rect { height: 1, ..area }, buf);
}

/// The bottom line: the transient status when there is one, the key legend
/// otherwise. One implementation for every mode, so a key reads the same
/// wherever it is offered.
fn render_keys(status: Option<&str>, keys: &[(&str, &str)], area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let dim = Style::new().fg(token::DIM);
    let key = Style::new().fg(token::MUTED);
    let line = match status {
        Some(status) => Line::from(Span::styled(format!(" {status}"), key)),
        None => {
            let mut spans = vec![Span::raw(" ")];
            for (i, (k, label)) in keys.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" · ", dim));
                }
                spans.push(Span::styled((*k).to_string(), key));
                spans.push(Span::styled(format!(" {label}"), dim));
            }
            Line::from(spans)
        }
    };
    Paragraph::new(line).render(Rect { height: 1, ..area }, buf);
}

const BROWSE_KEYS: [(&str, &str); 6] = [
    ("↵", "edit"),
    ("a", "assume"),
    ("n", "new"),
    ("x x", "delete"),
    ("v", "versions"),
    ("r", "reload"),
];

fn render_list(panel: &InstalledPanel, accessible: bool, area: Rect, buf: &mut Buffer) {
    let (head_area, list_area, foot_area) = bands(area);
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);

    let mut head = vec![
        Span::styled("agents", text),
        Span::styled(format!(" · {} installed", panel.entries.len()), muted),
    ];
    if let Some(assumed) = &panel.assumed {
        head.push(Span::styled(" · lead is ", muted));
        head.push(Span::styled(assumed.clone(), Style::new().fg(token::GOLD)));
    }
    render_head(head, head_area, buf);

    if panel.entries.is_empty() {
        let hint = if panel.busy {
            "loading installed agents…"
        } else if panel.loaded {
            "no agents installed — n creates one from a prompt"
        } else {
            "r loads the installed agents"
        };
        if list_area.height > 0 {
            let y = list_area.y + list_area.height.saturating_sub(1) / 2;
            Paragraph::new(hint)
                .style(muted)
                .alignment(Alignment::Center)
                .render(Rect::new(list_area.x, y, list_area.width, 1), buf);
        }
        render_keys(panel.status.as_deref(), &BROWSE_KEYS, foot_area, buf);
        return;
    }

    // Accessible mode: the same five fields, labelled, one row per agent. The
    // grid's column heads are a legend for columns, and columns are
    // whitespace to a reader.
    if accessible {
        if list_area.height > 0 && list_area.width > 0 {
            let width = list_area.width as usize;
            let lines: Vec<Line<'static>> = panel
                .entries
                .iter()
                .enumerate()
                .take(list_area.height as usize)
                .map(|(i, entry)| agent_record(entry, i == panel.sel, width))
                .collect();
            Paragraph::new(lines).render(list_area, buf);
        }
        render_keys(panel.status.as_deref(), &BROWSE_KEYS, foot_area, buf);
        return;
    }

    let header = Row::new(
        ["", "agent", "scope", "ver", "description", "toolbelt"]
            .into_iter()
            .map(|h| Cell::from(h).style(dim)),
    );
    let rows: Vec<Row> = panel
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            agent_row(
                entry,
                i == panel.sel,
                panel.assumed.as_deref() == Some(entry.name.as_str()),
            )
        })
        .collect();
    let widths = [
        Constraint::Length(2),  // mark
        Constraint::Length(20), // agent
        Constraint::Length(8),  // scope
        Constraint::Length(5),  // ver
        Constraint::Fill(3),    // description
        Constraint::Fill(2),    // toolbelt
    ];
    Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .render(list_area, buf);

    render_keys(panel.status.as_deref(), &BROWSE_KEYS, foot_area, buf);
}

/// One installed agent as `> name · scope project · version v2 · …`.
fn agent_record(entry: &InstalledAgentEntry, is_selected: bool, width: usize) -> Line<'static> {
    let fields = [
        ("scope", entry.scope.label().to_string()),
        ("version", format!("v{}", entry.version)),
        ("description", entry.description.clone()),
        ("toolbelt", toolbelt_label(&entry.tools)),
    ];
    crate::views::linear::record_line(
        crate::views::linear::identity(entry.name.clone(), is_selected, token::GOLD),
        &fields,
        width,
    )
}

fn agent_row(entry: &InstalledAgentEntry, is_selected: bool, assumed: bool) -> Row<'static> {
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    let mark = if assumed {
        Span::styled(glyph::SKILL.to_string(), Style::new().fg(token::GOLD))
    } else if is_selected {
        Span::styled(glyph::COLLAPSED.to_string(), Style::new().fg(token::GOLD))
    } else {
        Span::raw(" ")
    };
    let name = Span::styled(
        entry.name.clone(),
        if assumed {
            Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD)
        } else {
            text
        },
    );
    let mut row = Row::new(vec![
        Cell::from(Line::from(mark)),
        Cell::from(Line::from(name)),
        Cell::from(entry.scope.label()).style(muted),
        Cell::from(format!("v{}", entry.version)).style(muted),
        Cell::from(entry.description.clone()).style(text),
        Cell::from(toolbelt_label(&entry.tools)).style(muted),
    ]);
    if is_selected {
        row = row.style(Style::new().bg(token::HL));
    }
    row
}

/// The definition editor: the pinned version's full content in a textarea.
/// A save (ctrl+s) is ALWAYS a new version; the window follows the cursor.
fn render_editor(panel: &InstalledPanel, area: Rect, buf: &mut Buffer) {
    let (head_area, body, foot_area) = bands(area);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    let (name, scope) = match &panel.editing {
        Some((name, scope)) => (name.as_str(), scope.label()),
        None => ("agent", "?"),
    };
    render_head(
        vec![
            Span::styled("edit ", muted),
            Span::styled(name.to_string(), text),
            Span::styled(format!(" · {scope}"), muted),
        ],
        head_area,
        buf,
    );
    render_keys(
        None,
        &[("^S", "saves a NEW pinned version"), ("esc", "discards")],
        foot_area,
        buf,
    );
    if body.height == 0 || body.width == 0 {
        return;
    }

    let layout = composer::layout(&panel.editor, body.width as usize);
    let height = body.height as usize;
    // Scroll the window so the cursor row is always visible (bottom-anchored
    // once the content exceeds the viewport).
    let start = (layout.cursor_row + 1).saturating_sub(height);
    // Agent definitions are markdown (`<name>.md`), so rows highlight as
    // markdown source. Every row feeds the highlighter from the top so
    // fence/frontmatter state stays right regardless of the scroll window;
    // a soft-wrapped row scans as its own line (slightly-off coloring on a
    // wrapped heading, never a wrong character), matching the lexer's
    // degrade-gracefully contract.
    let mut hl = syntax::Highlighter::new(Some(syntax::Lang::Markdown));
    for (i, row) in layout.rows.iter().enumerate() {
        let spans = hl.spans(row, text);
        if i < start || i >= start + height {
            continue;
        }
        let y = body.y + (i - start) as u16;
        Paragraph::new(Line::from(spans)).render(Rect::new(body.x, y, body.width, 1), buf);
    }
    // The cursor cell, reversed — same visual as a terminal caret.
    let cy = body.y + (layout.cursor_row - start) as u16;
    let cx = body.x + (layout.cursor_col as u16).min(body.width.saturating_sub(1));
    if cy < body.y + body.height {
        buf.set_style(
            Rect::new(cx, cy, 1, 1),
            Style::default().add_modifier(Modifier::REVERSED),
        );
    }
}

/// Create-from-prompt, step 1: the description input.
fn render_create_describe(panel: &InstalledPanel, area: Rect, buf: &mut Buffer) {
    let (head_area, body, foot_area) = bands(area);
    let muted = Style::new().fg(token::MUTED);
    render_head(
        vec![
            Span::styled("new agent", Style::new().fg(token::TEXT)),
            Span::styled(" · describe what it should do", muted),
        ],
        head_area,
        buf,
    );
    render_keys(None, &[("⏎", "next"), ("esc", "cancel")], foot_area, buf);
    if body.height == 0 {
        return;
    }
    Paragraph::new(format!("> {}▏", panel.create_desc))
        .style(Style::new().fg(token::TEXT))
        .wrap(Wrap { trim: false })
        .render(body, buf);
    if body.height > 2 {
        let hint = "the session model drafts the definition (name, description, toolbelt, \
                    system prompt) from this description";
        Paragraph::new(hint).style(muted).render(
            Rect::new(body.x, body.y + body.height - 1, body.width, 1),
            buf,
        );
    }
}

/// Create-from-prompt, step 2: the install-scope picker.
fn render_create_scope(panel: &InstalledPanel, area: Rect, buf: &mut Buffer) {
    let (head_area, body, foot_area) = bands(area);
    render_head(
        vec![
            Span::styled("new agent", Style::new().fg(token::TEXT)),
            Span::styled(" · install scope", Style::new().fg(token::MUTED)),
        ],
        head_area,
        buf,
    );
    render_keys(
        None,
        &[("↑↓", "choose"), ("⏎", "create"), ("esc", "back")],
        foot_area,
        buf,
    );
    let options = [
        "project — .stella/agents (this workspace)",
        "user — ~/.stella/agents (all projects)",
    ];
    for (i, option) in options.iter().enumerate() {
        if (i as u16) >= body.height {
            break;
        }
        let selected = i == panel.scope_sel.min(1);
        let line = format!("{} {option}", if selected { ">" } else { " " });
        let style = if selected {
            Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(token::TEXT)
        };
        Paragraph::new(line)
            .style(style)
            .render(Rect::new(body.x, body.y + i as u16, body.width, 1), buf);
    }
}

/// Create-from-prompt, step 3: the in-flight creation view. Stays up — with
/// an animated spinner driven by the deck clock — until the driver's
/// completing [`crate::envelope::Inbound::AgentsList`] folds in. The status
/// line carries the driver's progress notes (e.g. "queued behind the running
/// turn").
fn render_creating(
    panel: &InstalledPanel,
    now_ms: u64,
    no_anim: bool,
    area: Rect,
    buf: &mut Buffer,
) {
    let (head_area, body, foot_area) = bands(area);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    render_head(
        vec![
            Span::styled("new agent", text),
            Span::styled(" · creating…", muted),
        ],
        head_area,
        buf,
    );
    render_keys(
        None,
        &[("esc", "hides (creation continues)")],
        foot_area,
        buf,
    );
    if body.height == 0 {
        return;
    }
    let spinner = crate::views::spinner_glyph(now_ms, no_anim);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{spinner} "),
                Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "Drafting the agent with the session model ({} scope)…",
                    panel.create_scope().label()
                ),
                text,
            ),
        ]),
        Line::default(),
        Line::from(Span::styled(
            format!("“{}”", panel.create_desc.trim()),
            muted,
        )),
        Line::default(),
        Line::from(Span::styled(
            "the new agent appears here the moment the draft installs",
            muted,
        )),
    ];
    if let Some(status) = &panel.status {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(status.clone(), muted)));
    }
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(body, buf);
}

/// Create-from-prompt, step 4: the settled creation view. On success the
/// just-created agent's full definition renders through Stella's own
/// markdown renderer — the SAME detail treatment as the skills tab's ctrl+o
/// preview — scrollable and clamped to content here (the key handler only
/// increments the offset). On failure the driver's error shows instead.
fn render_create_done(panel: &mut InstalledPanel, area: Rect, buf: &mut Buffer) {
    let (head_area, body, foot_area) = bands(area);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);

    // Failure: the outcome must be impossible to miss.
    if let Some(error) = panel.create_error.clone() {
        let red = Style::new().fg(token::RED);
        render_head(
            vec![
                Span::styled("new agent", text),
                Span::styled(" · creation failed", red),
            ],
            head_area,
            buf,
        );
        render_keys(None, &[("esc / ⏎", "close")], foot_area, buf);
        if body.height == 0 {
            return;
        }
        let lines = vec![
            Line::from(vec![
                Span::styled("✖ ", red.add_modifier(Modifier::BOLD)),
                Span::styled("The agent was not created", text),
            ]),
            Line::default(),
            Line::from(Span::styled(error, red)),
        ];
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(body, buf);
        return;
    }

    // Success: the created agent's detail view, looked up fresh in the
    // driver's snapshot (never a stale copy).
    let entry = panel
        .created_name
        .as_ref()
        .and_then(|name| panel.entries.iter().find(|e| &e.name == name))
        .cloned();
    let Some(entry) = entry else {
        // The name vanished between snapshots — degrade.
        render_head(
            vec![
                Span::styled("new agent", text),
                Span::styled(" · created", muted),
            ],
            head_area,
            buf,
        );
        render_keys(None, &[("esc / ⏎", "close")], foot_area, buf);
        Paragraph::new("created — but the entry is not in the latest list (press r to reload)")
            .style(muted)
            .render(body, buf);
        return;
    };

    render_head(
        vec![
            Span::styled(
                format!("{} created ", glyph::DONE),
                Style::new().fg(token::GREEN),
            ),
            Span::styled(entry.name.clone(), text),
        ],
        head_area,
        buf,
    );
    render_keys(
        None,
        &[("↑↓", "scroll"), ("esc / ⏎", "close")],
        foot_area,
        buf,
    );
    if body.height == 0 || body.width == 0 {
        return;
    }
    // A dim subtitle line (scope · version · toolbelt), then the scrollable
    // definition below — the skills preview's exact layout.
    let split = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(body);
    Paragraph::new(Line::from(Span::styled(
        format!(
            "{} · v{} · {}",
            entry.scope.label(),
            entry.version,
            toolbelt_label(&entry.tools)
        ),
        muted,
    )))
    .render(split[0], buf);
    let body_area = split[1];
    // Render through Stella's own theme-obeying markdown renderer (the same
    // one the transcript and the skills ctrl+o preview use), then clamp the
    // scroll to content so the last page stays reachable.
    let rendered = ratatui::text::Text::from(crate::markdown::render(&entry.content));
    let content_h = rendered.height();
    let max_scroll = content_h.saturating_sub(body_area.height as usize) as u16;
    let scroll = panel.created_scroll.min(max_scroll);
    panel.created_scroll = scroll;
    Paragraph::new(rendered)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
        .render(body_area, buf);
}

/// The version picker: every version on disk, the pinned one marked. ⏎
/// re-pins WITHOUT writing a new version.
fn render_version_picker(panel: &InstalledPanel, area: Rect, buf: &mut Buffer) {
    let Some(entry) = panel.selected() else {
        return;
    };
    let (head_area, body, foot_area) = bands(area);
    render_head(
        vec![
            Span::styled("versions ", Style::new().fg(token::MUTED)),
            Span::styled(entry.name.clone(), Style::new().fg(token::TEXT)),
        ],
        head_area,
        buf,
    );
    render_keys(
        None,
        &[("⏎", "pin (no new version)"), ("esc", "close")],
        foot_area,
        buf,
    );
    for (i, info) in entry.versions.iter().enumerate() {
        if (i as u16) >= body.height {
            break;
        }
        let selected = i == panel.version_sel;
        let pinned = if info.version == entry.version {
            "  ● pinned"
        } else {
            ""
        };
        let line = format!(
            "{} v{}  {}{pinned}",
            if selected { ">" } else { " " },
            info.version,
            info.label,
        );
        let mut style = if selected {
            Style::new().bg(token::HL).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(token::TEXT)
        };
        if info.version == entry.version {
            style = style.fg(token::GOLD);
        }
        Paragraph::new(line)
            .style(style)
            .render(Rect::new(body.x, body.y + i as u16, body.width, 1), buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{AgentScope, AgentVersionInfo};
    use crate::theme;

    fn buffer_text(buf: &Buffer) -> String {
        let area = *buf.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The style of the first cell of the first occurrence of `needle`.
    fn style_at(buf: &Buffer, needle: &str) -> Style {
        let area = *buf.area();
        let want: Vec<String> = needle.chars().map(|c| c.to_string()).collect();
        for y in 0..area.height {
            'col: for x in 0..area.width {
                for (k, w) in want.iter().enumerate() {
                    if buf.cell((x + k as u16, y)).map(|c| c.symbol()) != Some(w.as_str()) {
                        continue 'col;
                    }
                }
                return buf.cell((x, y)).expect("cell in area").style();
            }
        }
        panic!("{needle:?} not on screen");
    }

    fn entry(name: &str, tools: Option<Vec<String>>) -> InstalledAgentEntry {
        InstalledAgentEntry {
            name: name.into(),
            description: format!("what {name} does"),
            tools,
            scope: AgentScope::Project,
            source_path: format!("/ws/.stella/agents/{name}.md"),
            version: 2,
            versions: vec![
                AgentVersionInfo {
                    version: 1,
                    label: "2026-07-01".into(),
                },
                AgentVersionInfo {
                    version: 2,
                    label: "2026-07-16".into(),
                },
            ],
            content: format!("---\nname: {name}\n---\nbody"),
        }
    }

    fn ui_with(entries: Vec<InstalledAgentEntry>) -> DeckUi {
        let mut ui = DeckUi::default();
        ui.installed.entries = entries;
        ui.installed.loaded = true;
        ui
    }

    #[test]
    fn toolbelt_labels_are_honest_about_unrestricted_grants() {
        assert_eq!(toolbelt_label(&None), "all tools");
        assert_eq!(
            toolbelt_label(&Some(vec!["Read".into(), "Grep".into()])),
            "Read, Grep"
        );
    }

    #[test]
    fn list_renders_name_description_toolbelt_and_version() {
        let mut ui = ui_with(vec![
            entry("reviewer", Some(vec!["Read".into(), "Grep".into()])),
            entry("planner", None),
        ]);
        let area = Rect::new(0, 0, 120, 10);
        let mut buf = Buffer::empty(area);
        render(&mut ui, 0, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("reviewer"), "name shown:\n{text}");
        assert!(
            text.contains("what reviewer does"),
            "description shown:\n{text}"
        );
        assert!(text.contains("Read, Grep"), "toolbelt shown:\n{text}");
        assert!(
            text.contains("all tools"),
            "an unrestricted grant reads as `all tools`:\n{text}"
        );
        assert!(text.contains("v2"), "pinned version shown:\n{text}");
        assert!(text.contains("project"), "scope shown:\n{text}");
    }

    #[test]
    fn empty_loaded_list_hints_at_create_from_prompt() {
        let mut ui = ui_with(vec![]);
        let area = Rect::new(0, 0, 80, 8);
        let mut buf = Buffer::empty(area);
        render(&mut ui, 0, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("n creates one from a prompt"), "{text}");
    }

    #[test]
    fn editor_renders_the_loaded_content_and_the_save_contract() {
        let mut ui = ui_with(vec![entry("reviewer", None)]);
        ui.installed.mode = InstalledMode::Edit;
        ui.installed.editing = Some(("reviewer".into(), AgentScope::Project));
        ui.installed.editor.load("---\nname: reviewer\n---\nbody");
        let area = Rect::new(0, 0, 90, 10);
        let mut buf = Buffer::empty(area);
        render(&mut ui, 0, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("edit reviewer"), "{text}");
        assert!(
            text.contains("NEW pinned version"),
            "the save-is-a-new-version contract is on screen:\n{text}"
        );
        assert!(text.contains("name: reviewer"), "content shown:\n{text}");
    }

    #[test]
    fn editor_highlights_markdown_source() {
        let mut ui = ui_with(vec![entry("reviewer", None)]);
        ui.installed.mode = InstalledMode::Edit;
        ui.installed.editing = Some(("reviewer".into(), AgentScope::Project));
        ui.installed
            .editor
            .load("---\nname: reviewer\n---\n# Reviewer\nplain prose");
        let area = Rect::new(0, 0, 90, 10);
        let mut buf = Buffer::empty(area);
        render(&mut ui, 0, area, &mut buf);
        assert_eq!(
            style_at(&buf, "---").fg,
            Some(theme::SYNTAX_COMMENT),
            "frontmatter delimiter dims"
        );
        assert_eq!(
            style_at(&buf, "name:").fg,
            Some(theme::SYNTAX_KEYWORD),
            "frontmatter key lights up"
        );
        assert_eq!(
            style_at(&buf, "# Reviewer").fg,
            Some(theme::SYNTAX_KEYWORD),
            "heading lights up"
        );
        assert_eq!(
            style_at(&buf, "plain prose").fg,
            Some(token::TEXT),
            "prose keeps the body tone"
        );
    }

    #[test]
    fn version_picker_marks_the_pinned_version() {
        let mut ui = ui_with(vec![entry("reviewer", None)]);
        ui.installed.mode = InstalledMode::PickVersion;
        ui.installed.version_sel = 0;
        let area = Rect::new(0, 0, 80, 8);
        let mut buf = Buffer::empty(area);
        render(&mut ui, 0, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("v1"), "{text}");
        assert!(text.contains("v2  2026-07-16  ● pinned"), "{text}");
        assert!(
            text.contains("no new version"),
            "the pin-does-not-increment contract is on screen:\n{text}"
        );
    }

    #[test]
    fn create_flow_renders_description_then_scope() {
        let mut ui = ui_with(vec![]);
        ui.installed.mode = InstalledMode::CreateDescribe;
        ui.installed.create_desc = "reviews diffs".into();
        let area = Rect::new(0, 0, 100, 8);
        let mut buf = Buffer::empty(area);
        render(&mut ui, 0, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("reviews diffs"), "{text}");
        assert!(text.contains("describe what it should do"), "{text}");

        ui.installed.mode = InstalledMode::CreateScope;
        let mut buf = Buffer::empty(area);
        render(&mut ui, 0, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains(".stella/agents"), "{text}");
        assert!(text.contains("~/.stella/agents"), "{text}");
    }

    #[test]
    fn creating_view_shows_an_animated_spinner_and_the_description() {
        let mut ui = ui_with(vec![]);
        ui.installed.mode = InstalledMode::Creating;
        ui.installed.create_desc = "reviews diffs".into();
        ui.installed.status =
            Some("agent creation queued — it runs when the current turn finishes".into());
        let area = Rect::new(0, 0, 100, 10);

        let mut buf_a = Buffer::empty(area);
        render(&mut ui, 0, area, &mut buf_a);
        let text_a = buffer_text(&buf_a);
        assert!(text_a.contains("Drafting the agent"), "{text_a}");
        assert!(
            text_a.contains("reviews diffs"),
            "the description stays visible:\n{text_a}"
        );
        assert!(
            text_a.contains("creation queued"),
            "the driver's progress status shows:\n{text_a}"
        );

        // One spinner period later the frame differs — the spinner animates
        // off the deck clock the shell tick advances.
        let mut buf_b = Buffer::empty(area);
        render(&mut ui, 80, area, &mut buf_b);
        assert_ne!(
            buffer_text(&buf_a),
            buffer_text(&buf_b),
            "the spinner must advance with the deck clock"
        );

        // `--no-anim` pins it.
        ui.no_anim = true;
        let mut buf_c = Buffer::empty(area);
        render(&mut ui, 80, area, &mut buf_c);
        let mut buf_d = Buffer::empty(area);
        render(&mut ui, 160, area, &mut buf_d);
        assert_eq!(buffer_text(&buf_c), buffer_text(&buf_d));
    }

    #[test]
    fn create_done_view_renders_the_created_agents_definition() {
        let mut ui = ui_with(vec![entry("reviewer", None), entry("drafted", None)]);
        ui.installed.mode = InstalledMode::CreateDone;
        ui.installed.created_name = Some("drafted".into());
        let area = Rect::new(0, 0, 100, 12);
        let mut buf = Buffer::empty(area);
        render(&mut ui, 0, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("created drafted"), "header:\n{text}");
        assert!(
            text.contains("project · v2 · all tools"),
            "scope/version/toolbelt subtitle:\n{text}"
        );
        assert!(
            text.contains("name: drafted"),
            "the definition body renders — the ctrl+o detail treatment:\n{text}"
        );
    }

    #[test]
    fn create_done_view_shows_the_error_on_failure() {
        let mut ui = ui_with(vec![]);
        ui.installed.mode = InstalledMode::CreateDone;
        ui.installed.create_error = Some("agent creation failed: draft call failed: boom".into());
        let area = Rect::new(0, 0, 100, 10);
        let mut buf = Buffer::empty(area);
        render(&mut ui, 0, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("creation failed"), "{text}");
        assert!(
            text.contains("draft call failed: boom"),
            "the driver's error is on screen:\n{text}"
        );
    }

    /// The witness for the port (#4676): every mode of this tab draws its own
    /// content and no box around it. A border here would be the v1 chrome
    /// SPEC 5 reclaimed, re-spent one tab at a time.
    ///
    /// Corners and verticals are what it looks for, not `─`: a box always has
    /// all three, and the created agent's own markdown renders a `---` line as
    /// a horizontal rule, which is content.
    #[test]
    fn no_mode_draws_a_box_around_the_tab() {
        let modes = [
            InstalledMode::Browse,
            InstalledMode::Edit,
            InstalledMode::CreateDescribe,
            InstalledMode::CreateScope,
            InstalledMode::Creating,
            InstalledMode::CreateDone,
            InstalledMode::PickVersion,
        ];
        for mode in modes {
            let mut ui = ui_with(vec![entry("reviewer", None)]);
            ui.installed.mode = mode;
            ui.installed.editing = Some(("reviewer".into(), AgentScope::Project));
            ui.installed.created_name = Some("reviewer".into());
            ui.installed.editor.load("---\nname: reviewer\n---\nbody");
            let area = Rect::new(0, 0, 100, 14);
            let mut buf = Buffer::empty(area);
            render(&mut ui, 0, area, &mut buf);
            let text = buffer_text(&buf);
            for glyph in ['┌', '┐', '└', '┘', '│', '├', '┤', '╭', '╮', '╰', '╯']
            {
                assert!(
                    !text.contains(glyph),
                    "{mode:?} drew border glyph {glyph:?}:\n{text}"
                );
            }
        }
    }

    /// Colour comes from the token table, never from a hand-picked hex, and
    /// the accent is gold — the same gold the tab row lights the active tab
    /// with.
    #[test]
    fn the_assumed_agent_wears_the_token_gold() {
        let mut ui = ui_with(vec![entry("reviewer", None)]);
        ui.installed.assumed = Some("reviewer".into());
        let area = Rect::new(0, 0, 120, 8);
        let mut buf = Buffer::empty(area);
        render(&mut ui, 0, area, &mut buf);
        assert_eq!(style_at(&buf, "reviewer").fg, Some(token::GOLD));
    }
}
