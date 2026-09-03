// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The session-override pickers: `/model` (switch this session's model) and
//! `/agent` (run as an installed agent this session).
//!
//! ```text
//! ╭ model · 1/1 · this session only ─────────────────────────────╮
//! │  filter gpt-5.5▏                                             │
//! │ openrouter                                                   │
//! │▸ openrouter/openai/gpt-5.5                                   │
//! ╰────────────────── type to filter · ↑↓ move · ⏎ use · esc ────╯
//! ```
//!
//! One state machine ([`ListPicker`]) serves both overlays. It is a modal
//! list: `↑`/`↓` move (the deck's one list vocabulary,
//! [`crate::deck_ui::list_nav`]), typing narrows, `⏎` chooses, Esc cancels.
//! The two differ only in their rows and in what a choice sends.
//!
//! The rows are read LIVE at key and render time, off state the deck already
//! holds: the driver's [`EngineConfigState`] snapshot for models, the
//! INSTALLED AGENTS entries for agents. They are never copied into the
//! picker. Both snapshots can land *after* it opens, since opening sends the
//! refresh, and a copy taken then would pin it to an empty list.
//!
//! # Why it filters
//!
//! The list holds a model from every provider with a key. One gateway adds
//! hundreds. With OpenRouter set up it runs past four hundred rows, and the
//! window shows twelve. You cannot pick from that. The filter is what makes
//! it a list.
//!
//! It uses the same word and the same rule as the SETTINGS tab's model
//! picker ([`crate::views::engine_panel`] and its `picker_matches`). One
//! list, so one way to search it.
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
//! the two state fields; key routing is `deck_ui/pickers.rs`, which
//! `render_model`'s provider headings (`grouped_rows`) never reach.
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
    /// `⏎` on row `i` of the caller's **matching** list — the rows left after
    /// [`ListPicker::query`] narrows them, not the unfiltered candidates.
    Choose(usize),
    /// Esc — cancel, choosing nothing.
    Close,
}

/// The modal list state both pickers share. Rows live with the caller. This
/// holds whether the picker is up, where the highlight is, and the filter.
///
/// The caller owns the filtering. It passes the **matching** row count to
/// [`ListPicker::key`], and resolves [`PickerAction::Choose`] against the
/// same list it drew. So the bounds and the picked row always agree with
/// what the reader saw.
#[derive(Debug, Clone, Default)]
pub struct ListPicker {
    pub open: bool,
    sel: usize,
    query: String,
}

impl ListPicker {
    /// Raise the picker with the highlight on the first row and no filter.
    pub fn raise(&mut self) {
        self.open = true;
        self.sel = 0;
        self.query.clear();
    }

    /// Take the picker down, forgetting the filter — a picker reopened later
    /// is one the reader is opening fresh, not resuming.
    pub fn close(&mut self) {
        self.open = false;
        self.sel = 0;
        self.query.clear();
    }

    /// The highlighted row.
    #[must_use]
    pub fn selected(&self) -> usize {
        self.sel
    }

    /// What the reader has typed to narrow the list.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Fold one keystroke against a matching list `count` rows long.
    ///
    /// Typing edits the filter, so no letter is free for anything else. Esc
    /// is the way out. The arrows move, with `⇞`/`⇟`/`Home`/`End`. The
    /// SETTINGS tab's model picker uses that same vocabulary
    /// ([`crate::views::engine_panel`]), so one habit carries between them.
    pub fn key(&mut self, key: KeyEvent, count: usize) -> PickerAction {
        if !self.open {
            return PickerAction::Ignored;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return PickerAction::Ignored;
        }
        // The list can shrink between frames (a fresh snapshot landed, or the
        // filter narrowed it) — clamp before navigating so the highlight never
        // points past it.
        self.sel = self.sel.min(count.saturating_sub(1));
        // `letters` is false: they belong to the filter now.
        if crate::deck_ui::list_nav::select(key, &mut self.sel, count, false) {
            return PickerAction::Handled;
        }
        match key.code {
            KeyCode::Esc => PickerAction::Close,
            KeyCode::Enter if count > 0 => PickerAction::Choose(self.sel),
            KeyCode::Backspace => {
                self.query.pop();
                self.sel = 0; // the match set changed — re-anchor
                PickerAction::Handled
            }
            // ALT is allowed through, as it is on the SETTINGS picker: a
            // composed character arrives with it set on some layouts.
            KeyCode::Char(c)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META,
                ) =>
            {
                self.query.push(c);
                self.sel = 0; // the match set changed — re-anchor
                PickerAction::Handled
            }
            _ => PickerAction::Handled,
        }
    }
}

/// Case-insensitive substring match. The same rule as the SETTINGS tab's
/// `picker_matches` ([`crate::views::engine_panel`]), so a filter narrows
/// both pickers the same way. The caller trims and lowercases `needle`
/// once, not per row.
fn hits(haystack: &str, needle: &str) -> bool {
    needle.is_empty() || haystack.to_lowercase().contains(needle)
}

/// A query normalized for [`hits`]: trimmed, lowercased, once.
fn needle(query: &str) -> String {
    query.trim().to_lowercase()
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
        .map(crate::views::engine_panel::picker_candidates)
        .unwrap_or(&[])
}

/// [`model_candidates`] narrowed by what the reader has typed. The module
/// header says why the list needs a filter at all.
pub(crate) fn model_matches(ui: &DeckUi) -> Vec<String> {
    let needle = needle(ui.model_picker.query());
    model_candidates(ui)
        .iter()
        .filter(|spec| hits(spec, &needle))
        .cloned()
        .collect()
}

/// The `/agent` picker's matching rows, as indices into
/// `ui.installed.entries`. The caller needs each entry's scope as well as
/// its name, so an index serves it better than a copy.
///
/// Matched on the name *and* the description. A reader looks for an agent by
/// what it does as often as by what it is called.
pub(crate) fn agent_matches(ui: &DeckUi) -> Vec<usize> {
    let needle = needle(ui.agent_picker.query());
    ui.installed
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| hits(&entry.name, &needle) || hits(&entry.description, &needle))
        .map(|(i, _)| i)
        .collect()
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
///
/// `sel` and `count` are indices into whatever row space the caller is
/// windowing — the flat candidate list for [`render_agent`], or the
/// heading-interleaved painted rows [`grouped_rows`] builds for
/// [`render_model`]. The math does not care which; only the caller does.
fn window(sel: usize, count: usize) -> std::ops::Range<usize> {
    let visible = count.min(VISIBLE_ROWS);
    let start = (sel + 1)
        .saturating_sub(visible)
        .min(count.saturating_sub(visible));
    start..start + visible
}

/// One row the `/model` picker paints: a candidate at its flat index — the
/// same index [`PickerAction::Choose`] resolves against — or a
/// non-selectable provider heading. Mirrors the command palette's own
/// `PopupRow` (`render::display_rows`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum PickerRow {
    Heading(String),
    Candidate(usize),
}

/// The provider a `provider/slug` spec names: the text before the first
/// `/`. The same split [`typeahead_candidates`] and
/// `command_deck/model_cmd.rs` already depend on — a gateway spec is
/// `openrouter/openai/gpt-5.5`, and the provider is `openrouter`, not
/// `openai`. A spec with no `/` at all (malformed input the catalog should
/// never produce) is its own provider rather than a panic.
fn provider_of(spec: &str) -> &str {
    spec.split_once('/').map_or(spec, |(provider, _)| provider)
}

/// `candidates` — already filtered and ordered by the caller — as painted
/// rows: a heading inserted wherever the provider changes from the row
/// before it. This only marks the seams already present in the caller's
/// order; it does not re-sort, so two runs of the same provider split by a
/// different one draw that heading twice, the same seam-detection
/// [`crate::composer::SlashMenu::filter_with`] uses for its own domain
/// groups.
fn grouped_rows(candidates: &[String]) -> Vec<PickerRow> {
    let mut rows = Vec::with_capacity(candidates.len());
    let mut current: Option<&str> = None;
    for (i, spec) in candidates.iter().enumerate() {
        let provider = provider_of(spec);
        if current != Some(provider) {
            rows.push(PickerRow::Heading(provider.to_string()));
            current = Some(provider);
        }
        rows.push(PickerRow::Candidate(i));
    }
    rows
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

/// The card's first row: what has been typed, with the caret after it. The
/// word `filter` is the SETTINGS picker's, so the affordance reads the same
/// on both.
fn filter_row(query: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("  filter ", Style::new().fg(token::MUTED)),
        Span::styled(query.to_string(), Style::new().fg(token::TEXT)),
        Span::styled("▏", Style::new().fg(token::GOLD)),
    ])
}

/// Paint the `/model` picker. A no-op while closed.
pub fn render_model(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if !ui.model_picker.open {
        return;
    }
    let offered = model_candidates(ui).len();
    let candidates = model_matches(ui);
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

    let mut rows: Vec<Line<'static>> = vec![filter_row(ui.model_picker.query())];
    let mut selected_row = None;
    if offered == 0 {
        rows.push(Line::from(Span::styled(
            "no models to offer yet — waiting for the provider snapshot",
            muted,
        )));
        rows.push(Line::from(Span::styled(
            "(`/info` lists providers; `/info refresh` re-syncs the catalog)",
            dim,
        )));
    } else if candidates.is_empty() {
        // Which nothing this is. The snapshot has models; the filter hides
        // them.
        rows.push(Line::from(Span::styled(
            "no models match — Backspace to widen",
            muted,
        )));
    } else {
        // Windowed over the painted rows (headings included), not the
        // candidates — VISIBLE_ROWS caps what is drawn, and a heading takes
        // a row of its own. `window` itself is agnostic to which; only the
        // index space passed in differs from render_agent's below.
        let painted = grouped_rows(&candidates);
        let selected_painted = painted
            .iter()
            .position(|row| *row == PickerRow::Candidate(sel))
            .unwrap_or(0);
        let window = window(selected_painted, painted.len());
        for (row_i, row) in painted
            .iter()
            .enumerate()
            .skip(window.start)
            .take(window.len())
        {
            match row {
                PickerRow::Heading(provider) => {
                    // Chrome, not a candidate: no cursor column, dim like
                    // the palette's own section headings — never the row a
                    // `⏎` can land on.
                    rows.push(Line::from(Span::styled(format!(" {provider}"), dim)));
                }
                PickerRow::Candidate(i) => {
                    let spec = &candidates[*i];
                    let is_sel = *i == sel;
                    let mut spans = vec![
                        cursor(is_sel),
                        Span::styled(
                            truncate_cols(spec, inner_w.saturating_sub(12)),
                            name_style(is_sel),
                        ),
                    ];
                    // The session's live pin, as a WORD — the golden suite
                    // strips style, and this is the row a reader orients on.
                    if current.as_deref() == Some(spec.as_str()) {
                        spans.push(Span::styled("  · current", muted));
                    }
                    if is_sel {
                        // Offset by the filter row this list sits under.
                        selected_row = Some(row_i - window.start + 1);
                    }
                    rows.push(Line::from(spans));
                }
            }
        }
    }

    // Counted over the matches: that is the list the highlight walks.
    let position = (!candidates.is_empty())
        .then(|| format!("{}/{} · this session only", sel + 1, candidates.len()));
    render_card(
        Labels {
            name: "model",
            position,
            hints: "type to filter · ↑↓ move · ⏎ use · esc",
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
    let matching = agent_matches(ui);
    let muted = Style::new().fg(token::MUTED);
    let sel = ui
        .agent_picker
        .selected()
        .min(matching.len().saturating_sub(1));
    let inner_w = usize::from(card_w(area, ui.accessible)).saturating_sub(2);

    let mut rows: Vec<Line<'static>> = vec![filter_row(ui.agent_picker.query())];
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
    } else if matching.is_empty() {
        rows.push(Line::from(Span::styled(
            "no agents match — Backspace to widen",
            muted,
        )));
    } else {
        let window = window(sel, matching.len());
        for (row, &i) in matching
            .iter()
            .enumerate()
            .skip(window.start)
            .take(window.len())
        {
            let entry = &entries[i];
            let is_sel = row == sel;
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
                // Offset by the filter row this list sits under.
                selected_row = Some(row - window.start + 1);
            }
            rows.push(Line::from(spans));
        }
    }

    let position = (!matching.is_empty())
        .then(|| format!("{}/{} · this session only", sel + 1, matching.len()));
    render_card(
        Labels {
            name: "agent",
            position,
            hints: "type to filter · ↑↓ move · ⏎ assume · esc",
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

    /// The shared vocabulary: arrows move, Esc cancels, `⏎` chooses the
    /// highlighted row, and a closed picker claims nothing.
    #[test]
    fn the_picker_moves_chooses_and_cancels() {
        let mut picker = ListPicker::default();
        assert_eq!(picker.key(key(KeyCode::Enter), 3), PickerAction::Ignored);

        picker.raise();
        assert_eq!(picker.key(key(KeyCode::Down), 3), PickerAction::Handled);
        assert_eq!(picker.key(key(KeyCode::Down), 3), PickerAction::Handled);
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

    /// **The witness.** Typing narrows the list. It does not move in it. So
    /// every letter reaches the filter: `q` starts `qwen` instead of closing
    /// the card, and `j`/`k` type instead of moving. Esc is the way out. The
    /// arrows are the way around.
    #[test]
    fn letters_type_into_the_filter_rather_than_moving_or_quitting() {
        let mut picker = ListPicker::default();
        picker.raise();
        assert_eq!(picker.query(), "");

        for c in "qwen".chars() {
            assert_eq!(picker.key(key(KeyCode::Char(c)), 3), PickerAction::Handled);
        }
        assert_eq!(
            picker.query(),
            "qwen",
            "`q` belongs to the filter now, not to closing the card"
        );

        // `j`/`k` are letters like any other here.
        picker.key(key(KeyCode::Char('j')), 3);
        assert_eq!(picker.query(), "qwenj");
        assert_eq!(
            picker.key(key(KeyCode::Backspace), 3),
            PickerAction::Handled
        );
        assert_eq!(picker.query(), "qwen");

        // Esc still closes, and closing forgets the filter — a picker
        // reopened later is one being opened fresh.
        assert_eq!(picker.key(key(KeyCode::Esc), 3), PickerAction::Close);
        picker.close();
        picker.raise();
        assert_eq!(picker.query(), "");
    }

    /// Editing the filter re-anchors the highlight to the top: the index it
    /// held counts rows in the match set the edit just replaced, so carrying
    /// it over would land on an unrelated model.
    #[test]
    fn editing_the_filter_re_anchors_the_highlight() {
        let mut picker = ListPicker::default();
        picker.raise();
        picker.key(key(KeyCode::Down), 9);
        picker.key(key(KeyCode::Down), 9);
        assert_eq!(picker.selected(), 2);

        picker.key(key(KeyCode::Char('g')), 9);
        assert_eq!(picker.selected(), 0, "a typed letter re-anchors");

        picker.key(key(KeyCode::Down), 9);
        assert_eq!(picker.selected(), 1);
        picker.key(key(KeyCode::Backspace), 9);
        assert_eq!(picker.selected(), 0, "a Backspace re-anchors too");
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

    /// **The witness for the drawn filter.** The card puts the typed query
    /// on its own row. It draws only the rows that match. It counts the
    /// position over those: `1/1` where a filter kept one of three, not
    /// `1/3`. A filter that matches nothing says which nothing it is.
    #[test]
    fn the_card_draws_the_filter_and_only_the_rows_it_admits() {
        let model = WorkspaceModel::new();
        let mut ui = DeckUi::default();
        ui.engine.state = Some(EngineConfigState {
            catalog_models: vec![
                "zai/glm-5.2-air".to_string(),
                "anthropic/claude-fable-5".to_string(),
                "openrouter/qwen/qwen3-max".to_string(),
            ],
            ..Default::default()
        });
        ui.model_picker.raise();
        let area = Rect::new(0, 0, 100, 20);

        // Unfiltered: all three, counted over three.
        let mut buf = Buffer::empty(area);
        render_model(&model, &ui, area, &mut buf);
        let frame = text(&buf);
        assert!(
            frame.contains("filter ▏"),
            "the caret sits after it: {frame}"
        );
        assert!(frame.contains("· 1/3 ·"), "{frame}");

        // Typing `qwen` leaves one row, and the count follows the matches.
        for c in "qwen".chars() {
            ui.model_picker.key(key(KeyCode::Char(c)), 3);
        }
        let mut buf = Buffer::empty(area);
        render_model(&model, &ui, area, &mut buf);
        let frame = text(&buf);
        assert!(frame.contains("filter qwen▏"), "{frame}");
        assert!(frame.contains("▸ openrouter/qwen/qwen3-max"), "{frame}");
        assert!(
            !frame.contains("anthropic/claude-fable-5"),
            "a row the filter rejected must not be drawn: {frame}"
        );
        assert!(
            frame.contains("· 1/1 ·"),
            "counted over the matches: {frame}"
        );
        assert!(frame.contains("type to filter"), "{frame}");

        // A filter matching nothing names the filter as the cause, rather
        // than reading as "this workspace has no models".
        for c in "zzz".chars() {
            ui.model_picker.key(key(KeyCode::Char(c)), 1);
        }
        let mut buf = Buffer::empty(area);
        render_model(&model, &ui, area, &mut buf);
        let frame = text(&buf);
        assert!(frame.contains("no models match"), "{frame}");
        assert!(
            !frame.contains("waiting for the provider snapshot"),
            "that is the other nothing — an empty snapshot: {frame}"
        );
    }

    /// **The witness.** Five candidates split 3-and-2 across two providers
    /// paint seven rows — a heading before each run — and painted row 4
    /// (1-indexed: heading, candidate 0, candidate 1, *this one*) is
    /// candidate 2, the same flat index `PickerAction::Choose` resolves
    /// against (`crate::deck_ui::pickers`). Getting this mapping right is
    /// `grouped_rows`'s whole job; the render loop only walks it.
    #[test]
    fn grouped_rows_paint_a_heading_before_each_provider_run() {
        let candidates: Vec<String> = [
            "zai/glm-5.2-air",
            "zai/glm-4.7",
            "zai/glm-4.6-air",
            "anthropic/claude-fable-5",
            "anthropic/claude-fable-5-mini",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();

        let rows = grouped_rows(&candidates);
        assert_eq!(
            rows.len(),
            7,
            "5 candidates across 2 providers paint 7 rows: {rows:?}"
        );
        let headings = rows
            .iter()
            .filter(|row| matches!(row, PickerRow::Heading(_)))
            .count();
        assert_eq!(headings, 2, "{rows:?}");
        assert_eq!(
            rows[3],
            PickerRow::Candidate(2),
            "painted row 4 (1-indexed) is candidate 2: {rows:?}"
        );
    }

    /// A filter narrowed to one provider draws that provider's heading
    /// once — `grouped_rows` sees the already-filtered list, so a seam the
    /// filter removed cannot resurface in it.
    #[test]
    fn a_filter_narrowed_to_one_provider_draws_one_heading() {
        let candidates: Vec<String> = ["zai/glm-5.2-air", "zai/glm-4.7"]
            .into_iter()
            .map(str::to_string)
            .collect();
        assert_eq!(
            grouped_rows(&candidates),
            vec![
                PickerRow::Heading("zai".to_string()),
                PickerRow::Candidate(0),
                PickerRow::Candidate(1),
            ]
        );
    }

    /// **The witness for the drawn headings.** Two providers draw two
    /// headings on screen, arrowing past one still lands the highlight on
    /// the candidate the reader arrowed to, and a heading never carries the
    /// cursor marker — the row `⏎` could choose is a heading in no case.
    #[test]
    fn the_model_card_draws_a_heading_between_provider_runs() {
        let model = WorkspaceModel::new();
        let mut ui = DeckUi::default();
        ui.engine.state = Some(EngineConfigState {
            catalog_models: vec![
                "zai/glm-5.2-air".to_string(),
                "zai/glm-4.7".to_string(),
                "anthropic/claude-fable-5".to_string(),
            ],
            ..Default::default()
        });
        ui.model_picker.raise();
        let area = Rect::new(0, 0, 100, 20);

        let mut buf = Buffer::empty(area);
        render_model(&model, &ui, area, &mut buf);
        let frame = text(&buf);
        // A heading row's own interior text — between the card's left and
        // right border, trimmed — rather than the whole screen line, which
        // still carries the border and the surrounding centering padding.
        let headings: Vec<&str> = frame
            .lines()
            .filter_map(|line| line.split('│').nth(1).map(str::trim))
            .filter(|inner| *inner == "zai" || *inner == "anthropic")
            .collect();
        assert_eq!(headings, vec!["zai", "anthropic"], "{frame}");

        // Arrow past the heading between the two runs onto the last
        // candidate (flat index 2) and confirm the highlight followed the
        // candidates, not the painted rows.
        ui.model_picker.key(key(KeyCode::Down), 3);
        ui.model_picker.key(key(KeyCode::Down), 3);
        assert_eq!(ui.model_picker.selected(), 2);
        let mut buf = Buffer::empty(area);
        render_model(&model, &ui, area, &mut buf);
        let frame = text(&buf);
        assert!(
            frame.contains("▸ anthropic/claude-fable-5"),
            "the highlight followed the arrows onto the candidate: {frame}"
        );
        let heading_line = frame
            .lines()
            .find(|line| line.split('│').nth(1).map(str::trim) == Some("anthropic"))
            .expect("the heading is still drawn");
        assert!(
            !heading_line.contains('▸'),
            "a heading never carries the cursor: {heading_line:?}"
        );
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
