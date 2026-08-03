//! The AGENTS tab's INSTALLED AGENTS pane: the agents configured on disk at
//! the user (`~/.stella/agents`) and project (`.stella/agents`)
//! levels — name, description, and toolbelt per row — plus the pane's modal
//! sub-views (the definition editor, the create-from-prompt flow, and the
//! version picker).
//!
//! Every color comes from [`crate::theme`]; the list content comes verbatim
//! from the driver's [`crate::envelope::Inbound::AgentsList`] snapshot held
//! on [`crate::deck_ui::InstalledPanel`] (no shadow state).

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Widget, Wrap};

use crate::composer;
use crate::deck_ui::{DeckUi, InstalledMode, InstalledPanel};
use crate::envelope::InstalledAgentEntry;
use crate::syntax;
use crate::theme;

/// Column headers for the browse list, matching `widths` in [`render_list`].
const HEADERS: [&str; 5] = ["Agent", "Scope", "Ver", "Description", "Toolbelt"];

pub fn render(ui: &mut DeckUi, now_ms: u64, area: Rect, buf: &mut Buffer) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let accessible = ui.accessible;
    match ui.installed.mode {
        InstalledMode::Browse => render_list(&ui.installed, accessible, area, buf),
        InstalledMode::Edit => render_editor(&ui.installed, area, buf),
        InstalledMode::CreateDescribe => render_create_describe(&ui.installed, area, buf),
        InstalledMode::CreateScope => render_create_scope(&ui.installed, area, buf),
        InstalledMode::Creating => render_creating(&ui.installed, now_ms, ui.no_anim, area, buf),
        InstalledMode::CreateDone => render_create_done(&mut ui.installed, area, buf),
        InstalledMode::PickVersion => render_version_picker(&ui.installed, area, buf),
    }
}

/// The toolbelt cell text: the granted tools, or the honest "all tools"
/// when the definition doesn't restrict them.
pub fn toolbelt_label(tools: &Option<Vec<String>>) -> String {
    match tools {
        None => "all tools".to_string(),
        Some(list) => list.join(", "),
    }
}

fn render_list(panel: &InstalledPanel, accessible: bool, area: Rect, buf: &mut Buffer) {
    let title = format!(" Installed Agents — {} on disk ", panel.entries.len());
    let block = Block::default().borders(Borders::ALL).title(title);

    let bands = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    let (list_area, foot_area) = (bands[0], bands[1]);

    if panel.entries.is_empty() {
        let inner = block.inner(list_area);
        block.render(list_area, buf);
        let hint = if panel.busy {
            "loading installed agents…"
        } else if panel.loaded {
            "no agents installed — press n to create one from a prompt"
        } else {
            "press r to load the installed agents"
        };
        if inner.height > 0 {
            let y = inner.y + inner.height.saturating_sub(1) / 2;
            Paragraph::new(hint)
                .style(theme::muted())
                .alignment(Alignment::Center)
                .render(Rect::new(inner.x, y, inner.width, 1), buf);
        }
        render_footer(panel, foot_area, buf);
        return;
    }

    // Accessible mode: the same five fields, labelled, one row per agent. The
    // grid's `Agent | Scope | Ver | Description | Toolbelt` header is a legend
    // for columns, and columns are whitespace to a reader.
    if accessible {
        let inner = block.inner(list_area);
        block.render(list_area, buf);
        if inner.height > 0 && inner.width > 0 {
            let width = inner.width as usize;
            let lines: Vec<Line<'static>> = panel
                .entries
                .iter()
                .enumerate()
                .take(inner.height as usize)
                .map(|(i, entry)| agent_record(entry, i == panel.sel, width))
                .collect();
            Paragraph::new(lines).render(inner, buf);
        }
        render_footer(panel, foot_area, buf);
        return;
    }

    let header = Row::new(HEADERS.iter().copied().map(Cell::from)).style(theme::accent());
    let rows: Vec<Row> = panel
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| agent_row(entry, i == panel.sel))
        .collect();
    let widths = [
        Constraint::Length(20), // Agent
        Constraint::Length(8),  // Scope
        Constraint::Length(5),  // Ver
        Constraint::Fill(3),    // Description
        Constraint::Fill(2),    // Toolbelt
    ];
    Table::new(rows, widths)
        .header(header)
        .block(block)
        .column_spacing(1)
        .render(list_area, buf);

    render_footer(panel, foot_area, buf);
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
        crate::views::linear::identity(entry.name.clone(), is_selected, theme::ACCENT),
        &fields,
        width,
    )
}

fn agent_row(entry: &InstalledAgentEntry, is_selected: bool) -> Row<'static> {
    let caret = if is_selected {
        Span::styled("> ", Style::default().fg(theme::ACCENT))
    } else {
        Span::raw("  ")
    };
    let name_cell = Cell::from(Line::from(vec![
        caret,
        Span::styled(entry.name.clone(), Style::default().fg(theme::ACCENT)),
    ]));
    let scope_cell = Cell::from(entry.scope.label()).style(theme::muted());
    let ver_cell = Cell::from(format!("v{}", entry.version)).style(theme::body());
    let desc_cell = Cell::from(entry.description.clone()).style(theme::body());
    let tools_cell = Cell::from(toolbelt_label(&entry.tools)).style(theme::muted());
    let mut row = Row::new(vec![name_cell, scope_cell, ver_cell, desc_cell, tools_cell]);
    if is_selected {
        row = row.style(Style::default().add_modifier(Modifier::REVERSED));
    }
    row
}

/// The bottom line: the transient status when set, the key legend otherwise.
fn render_footer(panel: &InstalledPanel, area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let text = match &panel.status {
        Some(status) => status.clone(),
        None => "↑/↓ select · ⏎ edit (new pinned version) · v versions · n new from prompt · \
                 r reload · ←/→ panes"
            .to_string(),
    };
    Paragraph::new(text).style(theme::muted()).render(area, buf);
}

/// The definition editor: the pinned version's full content in a textarea.
/// A save (ctrl+s) is ALWAYS a new version; the window follows the cursor.
fn render_editor(panel: &InstalledPanel, area: Rect, buf: &mut Buffer) {
    let (name, scope) = match &panel.editing {
        Some((name, scope)) => (name.as_str(), scope.label()),
        None => ("agent", "?"),
    };
    let title =
        format!(" Edit {name} ({scope}) — ctrl+s saves a NEW pinned version · esc discards ");
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(theme::ACCENT));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let layout = composer::layout(&panel.editor, inner.width as usize);
    let height = inner.height as usize;
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
        let spans = hl.spans(row, theme::body());
        if i < start || i >= start + height {
            continue;
        }
        let y = inner.y + (i - start) as u16;
        Paragraph::new(Line::from(spans)).render(Rect::new(inner.x, y, inner.width, 1), buf);
    }
    // The cursor cell, reversed — same visual as a terminal caret.
    let cy = inner.y + (layout.cursor_row - start) as u16;
    let cx = inner.x + (layout.cursor_col as u16).min(inner.width.saturating_sub(1));
    if cy < inner.y + inner.height {
        buf.set_style(
            Rect::new(cx, cy, 1, 1),
            Style::default().add_modifier(Modifier::REVERSED),
        );
    }
}

/// Create-from-prompt, step 1: the description input.
fn render_create_describe(panel: &InstalledPanel, area: Rect, buf: &mut Buffer) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" New Agent — describe what it should do · ⏎ next · esc cancel ")
        .border_style(Style::default().fg(theme::ACCENT));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height == 0 {
        return;
    }
    let text = format!("> {}▏", panel.create_desc);
    Paragraph::new(text)
        .style(theme::body())
        .wrap(Wrap { trim: false })
        .render(inner, buf);
    if inner.height > 2 {
        let hint = "the session model drafts the definition (name, description, toolbelt, \
                    system prompt) from this description";
        Paragraph::new(hint).style(theme::muted()).render(
            Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1),
            buf,
        );
    }
}

/// Create-from-prompt, step 2: the install-scope picker.
fn render_create_scope(panel: &InstalledPanel, area: Rect, buf: &mut Buffer) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" New Agent — install scope · ↑/↓ choose · ⏎ create · esc back ")
        .border_style(Style::default().fg(theme::ACCENT));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height == 0 {
        return;
    }
    let options = [
        "project — .stella/agents (this workspace)",
        "user — ~/.stella/agents (all projects)",
    ];
    for (i, option) in options.iter().enumerate() {
        if (i as u16) >= inner.height {
            break;
        }
        let selected = i == panel.scope_sel.min(1);
        let line = format!("{} {option}", if selected { ">" } else { " " });
        let style = if selected {
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            theme::body()
        };
        Paragraph::new(line)
            .style(style)
            .render(Rect::new(inner.x, inner.y + i as u16, inner.width, 1), buf);
    }
}

/// Create-from-prompt, step 3: the in-flight creation dialog. Stays up —
/// with an animated spinner driven by the deck clock — until the driver's
/// completing [`crate::envelope::Inbound::AgentsList`] folds in. The status
/// line carries the driver's progress notes (e.g. "queued behind the
/// running turn").
fn render_creating(
    panel: &InstalledPanel,
    now_ms: u64,
    no_anim: bool,
    area: Rect,
    buf: &mut Buffer,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" New Agent — creating… · esc hides (creation continues) ")
        .border_style(Style::default().fg(theme::ACCENT));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height == 0 {
        return;
    }
    let spinner = crate::views::spinner_glyph(now_ms, no_anim);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{spinner} "),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "Drafting the agent with the session model ({} scope)…",
                    panel.create_scope().label()
                ),
                theme::body(),
            ),
        ]),
        Line::default(),
        Line::from(Span::styled(
            format!("“{}”", panel.create_desc.trim()),
            theme::muted(),
        )),
        Line::default(),
        Line::from(Span::styled(
            "the new agent appears here the moment the draft installs",
            theme::muted(),
        )),
    ];
    if let Some(status) = &panel.status {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(status.clone(), theme::muted())));
    }
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(inner, buf);
}

/// Create-from-prompt, step 4: the settled creation dialog. On success the
/// just-created agent's full definition renders through Stella's own
/// markdown renderer — the SAME detail treatment as the skills tab's ctrl+o
/// preview — scrollable and clamped to content here (the key handler only
/// increments the offset). On failure the driver's error shows instead.
fn render_create_done(panel: &mut InstalledPanel, area: Rect, buf: &mut Buffer) {
    // Failure: the error dialog. The outcome must be impossible to miss.
    if let Some(error) = panel.create_error.clone() {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" New Agent — creation failed · esc / ⏎ close ")
            .border_style(Style::default().fg(theme::DANGER));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 {
            return;
        }
        let lines = vec![
            Line::from(vec![
                Span::styled(
                    "✖ ",
                    Style::default()
                        .fg(theme::DANGER)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("The agent was not created", theme::body()),
            ]),
            Line::default(),
            Line::from(Span::styled(error, Style::default().fg(theme::DANGER))),
        ];
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(inner, buf);
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
        // The name vanished between snapshots — degrade honestly.
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" New Agent — created · esc / ⏎ close ")
            .border_style(Style::default().fg(theme::ACCENT));
        let inner = block.inner(area);
        block.render(area, buf);
        Paragraph::new("created — but the entry is not in the latest list (press r to reload)")
            .style(theme::muted())
            .render(inner, buf);
        return;
    };

    let title = format!(" ✓ created {} — ↑/↓ scroll · esc / ⏎ close ", entry.name);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(theme::ACCENT));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    // A dim subtitle line (scope · version · toolbelt), then the scrollable
    // definition below — the skills preview's exact layout.
    let bands = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
    Paragraph::new(Line::from(Span::styled(
        format!(
            "{} · v{} · {}",
            entry.scope.label(),
            entry.version,
            toolbelt_label(&entry.tools)
        ),
        theme::muted(),
    )))
    .render(bands[0], buf);
    let body_area = bands[1];
    // Render through Stella's own theme-obeying markdown renderer (the same
    // one the transcript and the skills ctrl+o preview use), then clamp the
    // scroll to content so the last page stays reachable.
    let text = ratatui::text::Text::from(crate::markdown::render(&entry.content));
    let content_h = text.height();
    let max_scroll = content_h.saturating_sub(body_area.height as usize) as u16;
    let scroll = panel.created_scroll.min(max_scroll);
    panel.created_scroll = scroll;
    Paragraph::new(text)
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
    let title = format!(
        " Versions — {} · ⏎ pin (no new version) · esc close ",
        entry.name
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(theme::ACCENT));
    let inner = block.inner(area);
    block.render(area, buf);
    for (i, info) in entry.versions.iter().enumerate() {
        if (i as u16) >= inner.height {
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
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            theme::body()
        };
        if info.version == entry.version {
            style = style.fg(theme::ACCENT);
        }
        Paragraph::new(line)
            .style(style)
            .render(Rect::new(inner.x, inner.y + i as u16, inner.width, 1), buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{AgentScope, AgentVersionInfo};

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
        assert!(
            text.contains("press n to create one from a prompt"),
            "{text}"
        );
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
        assert!(text.contains("Edit reviewer"), "{text}");
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
            theme::body().fg,
            "prose keeps the body style"
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
        assert!(text.contains("created drafted"), "title in border:\n{text}");
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
}
