// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The session-override pickers: `/model` (switch this session's model) and
//! `/agent` (run as an installed agent this session).
//!
//! ```text
//! ╭ model · 1/3 · this session only ─────────────────────────────╮
//! │▸ zai/glm-5.2-air  · current                                  │
//! │  anthropic/claude-fable-5                                    │
//! │  openrouter/openai/gpt-5.5                                   │
//! ╰─────────────────────────────────────── ↑↓ move · ⏎ use · esc ╯
//! ```
//!
//! One state machine ([`ListPicker`]) serves both overlays — a modal
//! scrollable list, `↑`/`↓` (the deck's one list vocabulary,
//! [`crate::deck_ui::list_nav`]) to move, `⏎` to choose, Esc to cancel —
//! because the two differ only in what their rows are and what a choice
//! sends. The rows are read LIVE at key/render time from state the deck
//! already holds (the driver's [`EngineConfigState`] snapshot for models,
//! the INSTALLED AGENTS entries for agents), never copied into the picker:
//! both snapshots can arrive *after* the picker opens (opening sends the
//! refresh), and a copy taken at open time would pin the overlay to an
//! empty list.
//!
//! Both render into the deck's **content band**, not the whole frame, and the
//! card sits on its last row — over the transcript, a row above the prompt it
//! is about. The chrome under that band is not a fixed number of rows: the
//! composer grows with its draft and the status bar takes a second row for an
//! earned cache diagnosis, so a card placed by counting back from the frame's
//! foot lands on the prompt at some composer height. The band already knows
//! where it ends.
//!
//! In a file of its own under the god-file rule — `deck_ui.rs` pays only
//! the two state fields; key routing is `deck_ui/pickers.rs`.
//!
//! [`EngineConfigState`]: crate::envelope::EngineConfigState

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget};
use stella_tui_theme::token;

use crate::deck::{PipelineRole, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::views::cards::truncate_cols;

/// Both pickers' card width. Wider than the frame's other floats for the
/// reason the `/info` dialog runs wide: a `provider/vendor/slug` spec may
/// not elide.
const PICKER_CARD_W: u16 = 64;

/// Rows shown at once; longer lists scroll under the selection.
const VISIBLE_ROWS: usize = 12;

/// What a keystroke did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    /// Not the picker's key (it is closed, or the key is Ctrl-C — declined
    /// so the deck's quit branch still fires).
    Ignored,
    /// Consumed; the picker stays up.
    Handled,
    /// `⏎` on row `i` of the caller's candidate list.
    Choose(usize),
    /// Esc/`q` — cancel, choosing nothing.
    Close,
}

/// The modal list state both pickers share. Rows live with the caller;
/// this holds only whether the picker is up and where the highlight is.
#[derive(Debug, Clone, Default)]
pub struct ListPicker {
    pub open: bool,
    sel: usize,
}

impl ListPicker {
    /// Raise the picker with the highlight on the first row.
    pub fn raise(&mut self) {
        self.open = true;
        self.sel = 0;
    }

    /// Take the picker down.
    pub fn close(&mut self) {
        self.open = false;
        self.sel = 0;
    }

    /// The highlighted row.
    #[must_use]
    pub fn selected(&self) -> usize {
        self.sel
    }

    /// Fold one keystroke against a candidate list `count` rows long.
    pub fn key(&mut self, key: KeyEvent, count: usize) -> PickerAction {
        if !self.open {
            return PickerAction::Ignored;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return PickerAction::Ignored;
        }
        // The list can shrink between frames (a fresh snapshot landed) —
        // clamp before navigating so the highlight never points past it.
        self.sel = self.sel.min(count.saturating_sub(1));
        if crate::deck_ui::list_nav::closes(key) {
            return PickerAction::Close;
        }
        // Modal, so `letters` is true (#4370).
        if crate::deck_ui::list_nav::select(key, &mut self.sel, count, true) {
            return PickerAction::Handled;
        }
        match key.code {
            KeyCode::Enter if count > 0 => PickerAction::Choose(self.sel),
            _ => PickerAction::Handled,
        }
    }
}

/// The `/model` picker's vocabulary: what the SETTINGS tab's model picker
/// offers — `allowed_models` when a restriction is configured, else the
/// catalog scoped to credentialed providers. Empty until the driver's first
/// [`EngineConfigState`] snapshot lands (opening the picker requests one).
///
/// [`EngineConfigState`]: crate::envelope::EngineConfigState
pub(crate) fn model_candidates(ui: &DeckUi) -> &[String] {
    ui.engine
        .state
        .as_ref()
        .map(crate::v2::engine_panel::picker_candidates)
        .unwrap_or(&[])
}

/// The `/model` **argument menu's** vocabulary ([`crate::composer::args`]):
/// [`model_candidates`] narrowed to the session's active provider.
///
/// The picker and the typeahead read one list on purpose — a second source
/// would let the two disagree about what is pickable — but they scope it
/// differently, because they answer different questions. The picker is a
/// menu of everywhere this workspace can go, so it offers every credentialed
/// provider. The typeahead completes a spec the reader is already typing at
/// *this* session, so it offers this session's provider.
///
/// Falls back to the full list when the active provider contributes nothing
/// to it (a gateway spec whose prefix is the gateway, not the vendor), since
/// an empty menu would read as "no models" rather than "none here".
pub(crate) fn typeahead_candidates(model: &WorkspaceModel, ui: &DeckUi) -> Vec<String> {
    let all = model_candidates(ui);
    let Some(provider) = model
        .role_pins
        .get(&PipelineRole::Worker)
        .map(|pin| pin.provider.clone())
    else {
        return all.to_vec();
    };
    let scoped: Vec<String> = all
        .iter()
        .filter(|spec| {
            spec.strip_prefix(provider.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        })
        .cloned()
        .collect();
    if scoped.is_empty() {
        all.to_vec()
    } else {
        scoped
    }
}

/// The window of `count` rows that keeps `sel` visible: at most
/// [`VISIBLE_ROWS`], slid so the highlight never leaves it.
fn window(sel: usize, count: usize) -> std::ops::Range<usize> {
    let visible = count.min(VISIBLE_ROWS);
    let start = (sel + 1)
        .saturating_sub(visible)
        .min(count.saturating_sub(visible));
    start..start + visible
}

/// The card's outer width: the whole frame in accessible mode, else capped
/// at [`PICKER_CARD_W`].
///
/// The float is a visual affordance, and a row clipped at its right border is
/// a row a screen reader never reaches the end of — so accessible mode takes
/// the frame and the rows elide against what they were actually given.
fn card_w(frame: Rect, accessible: bool) -> u16 {
    if accessible {
        frame.width
    } else {
        frame.width.min(PICKER_CARD_W)
    }
}

/// Where the card lands inside the content band `frame`: horizontally
/// centered, sitting on the band's last row, tall enough for `body_rows` plus
/// its two border rows and clamped to the band.
fn card_area(frame: Rect, body_rows: u16, accessible: bool) -> Rect {
    let w = card_w(frame, accessible);
    let h = (body_rows + 2).min(frame.height);
    Rect {
        x: frame.x + (frame.width.saturating_sub(w)) / 2,
        y: frame.y + frame.height.saturating_sub(h),
        width: w,
        height: h,
    }
}

/// The words a picker's border carries: its name and where the highlight
/// stands in the list along the top, the keys that move it along the bottom.
struct Labels<'a> {
    name: &'a str,
    /// `1/3 · this session only` — absent while the list has no rows to
    /// count, where a position would be a fraction of nothing.
    position: Option<String>,
    hints: &'a str,
}

/// Paint one picker's card: the rounded frame, its [`Labels`] set into the
/// border, and the rows between.
///
/// The selection is a `▸` marker **plus** the row tint. The golden suite
/// strips style, so a tint on its own would be invisible to it.
fn render_card(
    labels: Labels<'_>,
    rows: Vec<Line<'static>>,
    selected_row: Option<usize>,
    accessible: bool,
    area: Rect,
    buf: &mut Buffer,
) {
    let height = u16::try_from(rows.len()).unwrap_or(u16::MAX);
    let card = card_area(area, height, accessible);
    Clear.render(card, buf);

    let mut rows = rows;
    if let Some(sel) = selected_row
        && let Some(line) = rows.get_mut(sel)
    {
        line.style = line.style.bg(token::HL);
    }

    let mut title = vec![Span::styled(
        format!(" {}", labels.name),
        Style::new().fg(token::TEXT).add_modifier(Modifier::BOLD),
    )];
    if let Some(position) = labels.position {
        title.push(Span::styled(
            format!(" · {position}"),
            Style::new().fg(token::MUTED),
        ));
    }
    title.push(Span::raw(" "));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(token::BORDER))
        .title(Line::from(title))
        .title_bottom(
            Line::from(Span::styled(
                format!(" {} ", labels.hints),
                Style::new().fg(token::DIM),
            ))
            .right_aligned(),
        );
    Paragraph::new(rows).block(block).render(card, buf);
}

/// The row cursor: the marker in gold on the selected row, its width in
/// blanks on every other, so the column under it holds.
fn cursor(selected: bool) -> Span<'static> {
    Span::styled(
        if selected { "▸ " } else { "  " }.to_string(),
        Style::new().fg(token::GOLD),
    )
}

/// The style a row's own name takes: gold and bold under the cursor, the
/// plain text tone elsewhere.
fn name_style(selected: bool) -> Style {
    if selected {
        Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(token::TEXT)
    }
}

/// Paint the `/model` picker. A no-op while closed.
pub fn render_model(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if !ui.model_picker.open {
        return;
    }
    let candidates = model_candidates(ui);
    let current = model
        .role_pins
        .get(&PipelineRole::Worker)
        .map(crate::deck::RolePin::slug);
    let muted = Style::new().fg(token::MUTED);
    let dim = Style::new().fg(token::DIM);
    let sel = ui
        .model_picker
        .selected()
        .min(candidates.len().saturating_sub(1));
    let inner_w = usize::from(card_w(area, ui.accessible)).saturating_sub(2);

    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut selected_row = None;
    if candidates.is_empty() {
        rows.push(Line::from(Span::styled(
            "no models to offer yet — waiting for the provider snapshot",
            muted,
        )));
        rows.push(Line::from(Span::styled(
            "(`/info` lists providers; `/info refresh` re-syncs the catalog)",
            dim,
        )));
    } else {
        let window = window(sel, candidates.len());
        for (i, spec) in candidates
            .iter()
            .enumerate()
            .skip(window.start)
            .take(window.len())
        {
            let is_sel = i == sel;
            let mut spans = vec![
                cursor(is_sel),
                Span::styled(
                    truncate_cols(spec, inner_w.saturating_sub(12)),
                    name_style(is_sel),
                ),
            ];
            // The session's live pin, as a WORD — the golden suite strips
            // style, and this is the row a reader orients on.
            if current.as_deref() == Some(spec.as_str()) {
                spans.push(Span::styled("  · current", muted));
            }
            if is_sel {
                selected_row = Some(i - window.start);
            }
            rows.push(Line::from(spans));
        }
    }

    let position = (!candidates.is_empty())
        .then(|| format!("{}/{} · this session only", sel + 1, candidates.len()));
    render_card(
        Labels {
            name: "model",
            position,
            hints: "↑↓ move · ⏎ use · esc",
        },
        rows,
        selected_row,
        ui.accessible,
        area,
        buf,
    );
}

/// Paint the `/agent` picker. A no-op while closed.
pub fn render_agent(ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if !ui.agent_picker.open {
        return;
    }
    let entries = &ui.installed.entries;
    let muted = Style::new().fg(token::MUTED);
    let sel = ui
        .agent_picker
        .selected()
        .min(entries.len().saturating_sub(1));
    let inner_w = usize::from(card_w(area, ui.accessible)).saturating_sub(2);

    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut selected_row = None;
    if entries.is_empty() {
        rows.push(Line::from(Span::styled(
            if ui.installed.busy {
                "loading installed agents…"
            } else {
                "no installed agents — create one on the AGENTS tab (`/agents`)"
            },
            muted,
        )));
    } else {
        let window = window(sel, entries.len());
        for (i, entry) in entries
            .iter()
            .enumerate()
            .skip(window.start)
            .take(window.len())
        {
            let is_sel = i == sel;
            let used = 2 + entry.name.chars().count() + 2 + entry.scope.label().len() + 2;
            let spans = vec![
                cursor(is_sel),
                Span::styled(entry.name.clone(), name_style(is_sel)),
                Span::styled(format!("  {}", entry.scope.label()), muted),
                Span::styled(
                    format!(
                        "  {}",
                        truncate_cols(&entry.description, inner_w.saturating_sub(used))
                    ),
                    muted,
                ),
            ];
            if is_sel {
                selected_row = Some(i - window.start);
            }
            rows.push(Line::from(spans));
        }
    }

    let position =
        (!entries.is_empty()).then(|| format!("{}/{} · this session only", sel + 1, entries.len()));
    render_card(
        Labels {
            name: "agent",
            position,
            hints: "↑↓ move · ⏎ assume · esc",
        },
        rows,
        selected_row,
        ui.accessible,
        area,
        buf,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{AgentScope, EngineConfigState, InstalledAgentEntry};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn agent(name: &str, scope: AgentScope, description: &str) -> InstalledAgentEntry {
        InstalledAgentEntry {
            name: name.to_string(),
            description: description.to_string(),
            tools: None,
            scope,
            source_path: format!(".stella/agents/{name}.md"),
            version: 1,
            versions: Vec::new(),
            content: String::new(),
        }
    }

    fn text(buf: &Buffer) -> String {
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

    /// The shared vocabulary: arrows and `j`/`k` move, Esc/`q` cancel, `⏎`
    /// chooses the highlighted row, and a closed picker claims nothing.
    #[test]
    fn the_picker_moves_chooses_and_cancels() {
        let mut picker = ListPicker::default();
        assert_eq!(picker.key(key(KeyCode::Enter), 3), PickerAction::Ignored);

        picker.raise();
        assert_eq!(picker.key(key(KeyCode::Down), 3), PickerAction::Handled);
        assert_eq!(
            picker.key(key(KeyCode::Char('j')), 3),
            PickerAction::Handled
        );
        assert_eq!(picker.key(key(KeyCode::Enter), 3), PickerAction::Choose(2));
        // `Home`/`End` come with the shared vocabulary (#4370), not a
        // hand-rolled arrow pair.
        assert_eq!(picker.key(key(KeyCode::Home), 3), PickerAction::Handled);
        assert_eq!(picker.key(key(KeyCode::Enter), 3), PickerAction::Choose(0));
        assert_eq!(picker.key(key(KeyCode::End), 3), PickerAction::Handled);
        assert_eq!(picker.key(key(KeyCode::Enter), 3), PickerAction::Choose(2));
        assert_eq!(picker.key(key(KeyCode::Esc), 3), PickerAction::Close);
        assert_eq!(
            picker.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), 3),
            PickerAction::Ignored,
            "a picker must not be the one state you cannot quit from"
        );
    }

    /// A list that shrank between frames clamps the highlight, and `⏎` on
    /// an empty list chooses nothing.
    #[test]
    fn a_shrunken_or_empty_list_never_chooses_past_the_end() {
        let mut picker = ListPicker::default();
        picker.raise();
        for _ in 0..5 {
            picker.key(key(KeyCode::Down), 6);
        }
        assert_eq!(picker.key(key(KeyCode::Enter), 2), PickerAction::Choose(1));
        assert_eq!(picker.key(key(KeyCode::Enter), 0), PickerAction::Handled);
    }

    /// The window slides so the highlight stays visible at either end.
    #[test]
    fn the_window_keeps_the_selection_visible() {
        assert_eq!(window(0, 5), 0..5, "a short list shows whole");
        assert_eq!(window(0, 40), 0..VISIBLE_ROWS);
        let end = window(39, 40);
        assert!(end.contains(&39), "the last row is reachable");
        assert_eq!(end.len(), VISIBLE_ROWS);
        let mid = window(20, 40);
        assert!(mid.contains(&20));
    }

    /// **The witness for the SPEC 5 card.** The `/model` picker draws the
    /// rounded frame the deck's other overlays draw, names its position in
    /// the top border and its keys in the bottom one, and marks the live pin
    /// as a word. The selected row carries the marker **and** the tint.
    #[test]
    fn the_model_card_is_the_rounded_v2_frame() {
        let model = WorkspaceModel::new();
        let mut ui = DeckUi::default();
        ui.engine.state = Some(EngineConfigState {
            catalog_models: vec![
                "zai/glm-5.2-air".to_string(),
                "anthropic/claude-fable-5".to_string(),
            ],
            ..Default::default()
        });
        ui.model_picker.raise();

        let area = Rect::new(0, 0, 100, 20);
        let mut buf = Buffer::empty(area);
        render_model(&model, &ui, area, &mut buf);
        let frame = text(&buf);

        assert!(
            frame.contains("╭ model · 1/2 · this session only"),
            "{frame}"
        );
        assert!(frame.contains("↑↓ move · ⏎ use · esc"), "{frame}");
        assert!(frame.contains("▸ zai/glm-5.2-air"), "{frame}");
        assert!(frame.contains("  anthropic/claude-fable-5"), "{frame}");

        let (y, row) = frame
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains("▸ zai/glm-5.2-air"))
            .expect("the selected row is drawn");
        let x = row.find('▸').expect("the marker is on this row") as u16;
        assert_eq!(
            buf.cell((x, y as u16)).map(|c| c.fg),
            Some(token::GOLD),
            "the marker is gold: {row}"
        );
        assert_eq!(
            buf.cell((x + 2, y as u16)).map(|c| c.bg),
            Some(token::HL),
            "…and the row it marks is tinted: {row}"
        );
    }

    /// **The witness for the anchor.** The card sits on the last row of the
    /// band it is handed, whatever that band's height — which is what makes
    /// it land a row above the prompt without counting the deck's chrome. Two
    /// bands of different heights, the same offset from the foot.
    #[test]
    fn the_card_sits_on_the_last_row_of_the_band_it_is_given() {
        let model = WorkspaceModel::new();
        let mut ui = DeckUi::default();
        ui.engine.state = Some(EngineConfigState {
            catalog_models: vec!["zai/glm-5.2-air".to_string()],
            ..Default::default()
        });
        ui.model_picker.raise();

        for height in [20u16, 14] {
            let band = Rect::new(0, 1, 100, height);
            let mut buf = Buffer::empty(Rect::new(0, 0, 100, height + 1));
            render_model(&model, &ui, band, &mut buf);
            let frame = text(&buf);
            let bottom = frame
                .lines()
                .position(|l| l.contains('╰'))
                .expect("the card's foot is drawn");
            assert_eq!(
                bottom,
                usize::from(band.bottom() - 1),
                "the card's foot is the band's last row at height {height}:\n{frame}"
            );
        }
    }

    /// The `/agent` picker names each definition's scope and what it is for,
    /// and says so plainly when nothing is installed.
    #[test]
    fn the_agent_card_lists_scope_and_purpose() {
        let mut ui = DeckUi::default();
        ui.agent_picker.raise();
        let area = Rect::new(0, 0, 100, 20);

        let mut buf = Buffer::empty(area);
        render_agent(&ui, area, &mut buf);
        assert!(text(&buf).contains("no installed agents"), "{}", text(&buf));

        ui.installed.entries = vec![agent(
            "reviewer",
            AgentScope::Project,
            "reviews a diff against the house rules",
        )];
        let mut buf = Buffer::empty(area);
        render_agent(&ui, area, &mut buf);
        let frame = text(&buf);
        assert!(
            frame.contains("╭ agent · 1/1 · this session only"),
            "{frame}"
        );
        assert!(
            frame.contains("▸ reviewer  project  reviews a diff against the house rules"),
            "{frame}"
        );
        assert!(frame.contains("↑↓ move · ⏎ assume · esc"), "{frame}");
    }

    /// Accessible mode takes the frame instead of the float, so a row elides
    /// against the width it was actually given rather than against the card
    /// cap a screen reader cannot see.
    #[test]
    fn accessible_mode_spans_the_frame() {
        let mut ui = DeckUi {
            accessible: true,
            ..Default::default()
        };
        ui.agent_picker.raise();
        ui.installed.entries = vec![agent(
            "release-captain",
            AgentScope::User,
            "cuts a release: bump the version, write the changelog, tag it, push it, \
             and watch the workflow to green",
        )];
        let area = Rect::new(0, 0, 120, 20);
        let mut buf = Buffer::empty(area);
        render_agent(&ui, area, &mut buf);
        let frame = text(&buf);
        assert!(
            frame
                .lines()
                .any(|l| l.starts_with('╭') && l.chars().count() == 120),
            "the card spans the frame:\n{frame}"
        );
        assert!(
            frame.contains("write the changelog"),
            "…and the row uses the width it was given:\n{frame}"
        );
    }
}
