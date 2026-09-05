// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The model-routing dialog (`/info`, and `/models` before the rename, which
//! still routes here): every role's **resolved** wiring —
//! the model it will send, the effort and thinking riding with it, and the
//! setting that decided each.
//!
//! # What this replaced, and why
//!
//! It replaced a three-slot card that printed `think` / `work` / `verify` and
//! one `provider/model` slug apiece. A slug alone cannot answer what anybody
//! actually opens this for: *why* is the verifier on that model, is the
//! `effort` I pinned still in force, which of four settings keys do I edit to
//! change it. Those were one-inference-from-silence questions, and this
//! dialog exists to make them readable instead.
//!
//! It also replaced a permanent `T·… W·… J·…` row of pins. Routing is a
//! thing you *check*, not a glanceable: the pins do not move during a session,
//! so a row that never changed was paying rent on every frame. The status bar
//! ([`crate::views::status_bar`]) keeps naming the pin that is actually answering
//! — its worker cell — and nothing more.
//!
//! # It renders; it does not resolve
//!
//! Every cell arrives pre-rendered on
//! [`crate::envelope::EngineConfigState::roles`], resolved driver-side by the
//! module that mirrors the request path's own precedence — `stella config`
//! prints these same cells from the same function. A dialog that re-derived
//! any of it would be a second answer free to drift from the engine's, and a
//! routing report that disagrees with what runs is worse than no report.
//!
//! Because that snapshot is seeded at session start, the dialog is complete on
//! the frame it opens: no round-trip, and nothing to wait for.
//!
//! # Running now, versus saved for next time
//!
//! The cells are what **this session** resolved, and they stay that way: a
//! running session keeps the wiring it resolved at start, and printing a
//! mid-session settings edit as though it were in force would misreport the
//! exact thing the dialog exists to answer.
//!
//! But saying nothing about a saved edit was its own way of lying. Someone
//! who changed a model pin in the ENGINE panel, saved, and opened this dialog
//! saw the **old** one, with no note — and the panel's own
//! "applies to runs started from now on" line is one tab away and gone by
//! then. The dialog read as having ignored the save (#1521).
//!
//! So the driver resolves the wiring a second time, from the settings as they
//! sit on disk, and sends the cells that differ as
//! [`RoleWiringRow::next_session`]. The dialog names both: the row is what is
//! running, a WARN-toned line under it is what a session started now would
//! get, and the title counts them so the pending edit is visible on the frame
//! this opens.
//!
//! # Copy law (D6)
//!
//! The interactive agent is `lead`, the word the deck already uses for that
//! lane. It is the only word this file knows: the core loop resolves exactly
//! one role (`default`). The three pipeline slots this section once named
//! (`think` / `work` / `verify`) went with the settings keys that stopped
//! steering anything, and the table naming them is gone with them.
//!
//! A role the deck has no word for — one a host contributed, which the table
//! is open to since #3472 — is named by its own key. That is the one place an
//! identifier is a label, and nothing else here is true: the deck cannot
//! invent a word for a role it has never heard of, and a category label
//! ("plugin") would name the row after where it came from, not after itself.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use stella_tui_theme::token;
use unicode_width::UnicodeWidthStr;

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;
use crate::envelope::{EngineConfigState, RoleWiringRow, role_table};
use crate::render::columns;
use crate::theme;
use crate::views::cards;

/// Wider than [`cards::CARD_MAX_W`], the only card that is: a model slug and
/// the settings key that chose it are fixed strings that lose their meaning
/// elided, and `openrouter/moonshotai/kimi-k3` alone would spend 29 of the
/// standard card's 52 usable columns.
const MODELS_CARD_W: u16 = 72;

/// The narrowest the dimmed left label column ever gets — a floor, not the
/// width.
///
/// It was the width while `default` was the only word the driver could send.
/// A plugin names its own seat (`<plugin-id>/<role>`), so no constant here can
/// know how wide the widest word is, and padding to a fixed minimum does not
/// cut: a wider word abutted the model column with no gap and pushed the line
/// past the card's own border. [`label_w`] measures instead.
const LABEL_W: usize = 10;

/// Columns kept between the label and the model, so the two never abut.
const LABEL_GAP: usize = 2;

/// The header's own label words, in the order [`header_rows`] draws them.
///
/// Named here so [`render`] can measure them into the same label column the
/// role rows use without restating what the header says.
const HEADER_LABELS: [&str; 2] = ["session", "auto"];

/// The label column for the table being drawn: its widest word plus
/// [`LABEL_GAP`], floored at [`LABEL_W`] and capped at half the card so the
/// model column cannot be squeezed out by one long seat key.
///
/// Measured over the words actually drawn, the way [`crate::views::seats`]'
/// `column_widths` measures its own rows. Every row of one table reads this
/// one answer — a header row padded to a different width than the role rows
/// beneath it is a table with two left edges.
fn label_w<'a>(words: impl Iterator<Item = &'a str>, inner_w: usize) -> usize {
    let widest = words.map(columns::width).max().unwrap_or(0);
    let cap = (inner_w / 2).max(LABEL_W);
    (widest + LABEL_GAP).clamp(LABEL_W, cap)
}

/// `word` laid into a label column of `label_w` columns.
///
/// Cut to fit before padding, because [`label_w`] is capped: a seat key wider
/// than the cap would otherwise widen its own row and shift the table.
fn label_cell(word: &str, label_w: usize) -> String {
    columns::pad(
        &cards::truncate_cols(word, label_w.saturating_sub(LABEL_GAP)),
        label_w,
    )
}

/// The only role this dialog has a **word** for: the settings key the driver
/// sends it under, and the word the deck says out loud for it.
///
/// Not the roles it prints. It prints every row the driver sends
/// ([`role_table`]); this is the one row whose word the deck has decided, and
/// a role outside it is printed after it under its own key. Adding a name
/// here is a copy decision, never a gate — and there is nothing to add until
/// the driver can resolve a role besides `default`.
///
/// Named six roles once — `think` / `research` / `plan` / `work` / `verify`
/// — for settings keys that had already stopped steering anything.
/// `role_table` still rendered every one of them, because it folds over
/// whatever the driver actually sent, and the only sender left was a fixture
/// built to spell those keys, never a real session.
const KNOWN: [(&str, &str); 1] = [("default", "lead")];

/// Pack `parts` onto as few `width`-column rows as they fit on, joined by the
/// deck's ` · ` separator.
///
/// Wrapped rather than truncated because of what these parts are: the longest
/// one is `high  (effort_auto replaced "max")`, and an ellipsis lands squarely
/// on the disclosure — the single most useful thing this dialog can say — the
/// moment the terminal is anything short of very wide. A part that cannot fit
/// a row on its own still gets elided; nothing else can be done with it.
///
/// **Not [`cards::wrap`]**, and the difference is why #5156 unified the
/// crate's other two copies and left this one alone. It packs *pre-split
/// parts* rather than breaking on whitespace, so a part is atomic and its own
/// spaces never become break points; it joins with the deck's ` · ` separator
/// rather than a space; and it **truncates** a part too wide for a row, where
/// `wrap` gives it a row of its own and leaves the overflow to the widget. It
/// measures display width, which `cards::wrap` does not (#5307).
fn wrap_parts(parts: &[String], width: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    for part in parts {
        match rows.last_mut() {
            Some(row) if row.width() + 3 + part.width() <= width => {
                row.push_str(" · ");
                row.push_str(part);
            }
            _ => rows.push(cards::truncate_cols(part, width)),
        }
    }
    rows
}

/// One role as a headline plus its detail: the word and its model, then the
/// effort, the thinking and the setting that chose the model.
///
/// Stacked rather than laid out as five table columns because these cells are
/// sentences, not values, and a column narrow enough to sit five abreast is
/// exactly the column that would cut them off.
fn role_rows(
    row: &RoleWiringRow,
    word: &str,
    inner_w: usize,
    label_w: usize,
    accessible: bool,
) -> Vec<Line<'static>> {
    let dim = Style::new().fg(token::MUTED);
    let parts = [
        row.effort.clone(),
        row.thinking.clone(),
        format!("from {}", row.source),
    ];
    let detail = parts.join(" · ");

    if accessible {
        // A labeled record per role: no column alignment to hear, and every
        // field carries its own name instead of a position in a grid.
        let text = format!("· {word} · model {} · {detail}", row.model);
        return vec![Line::from(Span::styled(text, Style::new().fg(token::TEXT)))];
    }

    let head = vec![
        Span::styled(label_cell(word, label_w), dim),
        Span::styled(
            cards::truncate_cols(&row.model, inner_w.saturating_sub(label_w)),
            Style::new().fg(token::TEXT),
        ),
    ];
    let mut rows = vec![Line::from(head)];
    for line in wrap_parts(&parts, inner_w.saturating_sub(label_w)) {
        rows.push(Line::from(vec![
            Span::raw(" ".repeat(label_w)),
            Span::styled(line, dim),
        ]));
    }
    rows.extend(next_session_row(row, inner_w, label_w, accessible));
    rows
}

/// The pending-edit row: what this role would resolve to in a session started
/// now, when a saved settings edit makes that differ.
///
/// In the WARN tone rather than the dim one every other detail row uses,
/// because it is the one line here that is *not* describing what is running —
/// and the failure it exists to prevent is a reader taking it for one that is.
fn next_session_row(
    row: &RoleWiringRow,
    inner_w: usize,
    label_w: usize,
    accessible: bool,
) -> Vec<Line<'static>> {
    let Some(next) = row.next_session.as_deref() else {
        return Vec::new();
    };
    let text = format!("next session: {next}");
    if accessible {
        return vec![Line::from(Span::styled(
            format!("· {text}"),
            Style::new().fg(theme::WARN),
        ))];
    }
    vec![Line::from(vec![
        Span::raw(" ".repeat(label_w)),
        Span::styled(
            cards::truncate_cols(&text, inner_w.saturating_sub(label_w)),
            Style::new().fg(theme::WARN),
        ),
    ])]
}

/// The header rows: what this session resolved to, and which auto-modes are
/// deciding for the roles below.
///
/// The auto line earns its place by being the explanation for the surprise
/// this dialog exists to surface — an effort reading `medium` under a pin of
/// `max` makes sense only once you can see that `effort_auto` is on.
fn header_rows(
    model: &WorkspaceModel,
    state: &EngineConfigState,
    label_w: usize,
    accessible: bool,
) -> Vec<Line<'static>> {
    let dim = Style::new().fg(token::MUTED);
    let on_off = |on: bool| if on { "on" } else { "off" };
    let autos = format!(
        "effort {} · thinking {}",
        on_off(state.effort_auto),
        on_off(state.reasoning_auto),
    );
    // The lead lane's own pin — what a role with no opinion of its own
    // inherits, and the thing every `session default` below points at.
    let session = model
        .agents
        .first()
        .and_then(|a| a.meta.model.clone())
        .unwrap_or_else(|| "—".to_string());
    let row = |label: &str, value: String| {
        if accessible {
            Line::from(Span::styled(
                format!("· {label} {value}"),
                Style::new().fg(token::TEXT),
            ))
        } else {
            Line::from(vec![
                Span::styled(label_cell(label, label_w), dim),
                Span::styled(value, Style::new().fg(token::TEXT)),
            ])
        }
    };
    vec![
        row(HEADER_LABELS[0], session),
        row(HEADER_LABELS[1], autos),
        Line::default(),
    ]
}

/// The card's body: the header rows, then one block per role the driver sent.
///
/// [`render`] and its tests both fold through here, so a test cannot pass on
/// rows the dialog would not print — and the label column cannot be measured
/// one way for the frame and another for the assertion.
fn body_rows(
    model: &WorkspaceModel,
    state: &EngineConfigState,
    inner_w: usize,
    accessible: bool,
) -> Vec<Line<'static>> {
    // Folded, not looked up (#3472). Looking each key up in [`KNOWN`] would
    // drop any row under a key it does not list. A role a plugin adds would
    // then run and spend while this dialog said it did not exist.
    let table = role_table(state, &KNOWN);
    // One width for the whole card, measured over the header's own labels as
    // well as the roles', so every row shares a left edge.
    let label_w = label_w(
        HEADER_LABELS
            .iter()
            .copied()
            .chain(table.iter().map(|entry| entry.word)),
        inner_w,
    );
    let mut rows = header_rows(model, state, label_w, accessible);
    for entry in &table {
        rows.extend(role_rows(
            entry.row, entry.word, inner_w, label_w, accessible,
        ));
    }
    rows
}

/// Render the model-routing dialog over `frame`.
pub fn render(model: &WorkspaceModel, ui: &DeckUi, frame: Rect, buf: &mut Buffer) {
    let dim = Style::new().fg(token::MUTED);
    let w = frame.width.min(MODELS_CARD_W);
    let inner_w = w.saturating_sub(4).max(LABEL_W as u16 + 8) as usize;

    // `pristine` is the last driver snapshot adopted verbatim, never the
    // ENGINE overlay's working copy — so an unsaved edit being typed one tab
    // over can never be printed here as though it were in force.
    let state = ui.engine.pristine.as_ref().or(ui.engine.state.as_ref());

    let mut rows: Vec<Line<'static>> = Vec::new();
    match state.filter(|s| !s.roles.is_empty()) {
        // The driver seeds this at startup, so an empty snapshot means the
        // answer is not in yet — say that rather than draw a plausible one.
        None => rows.push(Line::from(Span::styled(
            "routing has not been resolved yet",
            dim,
        ))),
        Some(state) => rows.extend(body_rows(model, state, inner_w, ui.accessible)),
    }

    // The title carries the count so a pending edit is visible on the frame
    // this opens, without reading four rows to find the one that moved.
    let pending = state.map_or(0, |s| {
        s.roles.iter().filter(|r| r.next_session.is_some()).count()
    });
    let mut title = vec![Span::styled("resolved routing", dim)];
    if pending > 0 {
        let phrase = if pending == 1 {
            "1 saved change applies".to_string()
        } else {
            format!("{pending} saved changes apply")
        };
        title.push(Span::styled(
            format!(" · {phrase} next session"),
            Style::new().fg(theme::WARN),
        ));
    }

    let area = cards::card_area(frame, rows.len() as u16, MODELS_CARD_W, ui.accessible);
    let inner = cards::card_frame(area, "models", title, "esc close", buf);
    cards::render_body(rows, None, inner, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wiring(role: &str, model: &str, effort: &str, source: &str) -> RoleWiringRow {
        RoleWiringRow {
            role: role.to_string(),
            model: model.to_string(),
            effort: effort.to_string(),
            thinking: "thinking on".to_string(),
            source: source.to_string(),
            next_session: None,
        }
    }

    /// The one row `resolve` can send today. The `effort_auto` disclosure
    /// rides on it rather than on a fabricated `verifier` row, so the wrap
    /// test below still exercises the card's longest line without pretending
    /// the driver can name a role it cannot.
    fn state() -> EngineConfigState {
        EngineConfigState {
            effort_auto: true,
            roles: vec![wiring(
                "default",
                "anthropic/claude-opus-5",
                "high  (effort_auto replaced \"max\")",
                "agents.default.model",
            )],
            ..Default::default()
        }
    }

    fn text_of(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The body `render` builds, without a terminal — the production fold
    /// itself, so a test cannot pass on rows the dialog would not print.
    fn body_of(model: &WorkspaceModel, state: &EngineConfigState, accessible: bool) -> String {
        text_of(&body_rows(model, state, 68, accessible))
    }

    fn rendered(model: &WorkspaceModel, accessible: bool) -> String {
        body_of(model, &state(), accessible)
    }

    /// The dialog's whole reason to exist: the role names the setting that
    /// chose its model, and an effort names what an auto-mode took from it. A
    /// slug alone is what the card this replaced already showed.
    #[test]
    fn every_role_names_its_model_its_effort_and_the_setting_that_chose_it() {
        let text = rendered(&WorkspaceModel::new(), false);
        for needle in [
            "agents.default.model",
            "effort_auto replaced",
            "anthropic/claude-opus-5",
            "thinking on",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
    }

    /// A saved settings edit is named, and named as *pending* (#1521).
    ///
    /// The dialog keeps printing what this session resolved — that is the
    /// question it answers — so the running pin must still be there. But a
    /// user who saved a new model and saw only the old one read the dialog as
    /// having ignored the save, which is the failure this row exists to
    /// prevent. Both answers, distinguishable.
    #[test]
    fn a_saved_edit_is_named_as_pending_without_displacing_what_is_running() {
        let mut row = wiring(
            "default",
            "anthropic/claude-opus-5",
            "high",
            "agents.default.model",
        );
        row.next_session = Some("openai/gpt-5.5".to_string());
        let lines = role_rows(&row, "lead", 68, LABEL_W, false);
        let text = text_of(&lines);

        assert!(
            text.contains("anthropic/claude-opus-5"),
            "the running pin is still the row's answer:\n{text}"
        );
        assert!(
            text.contains("next session: openai/gpt-5.5"),
            "the saved edit is named, and named as next-session:\n{text}"
        );
    }

    /// A role whose saved settings agree with what is running says nothing —
    /// a "next session" line on every row would be noise that trains the
    /// reader to skip the one row where it means something.
    #[test]
    fn a_role_with_no_pending_edit_stays_silent() {
        let row = wiring("default", "zai/glm-5.2", "medium", "default_model");
        let text = text_of(&role_rows(&row, "lead", 68, LABEL_W, false));
        assert!(!text.contains("next session"), "{text}");
    }

    /// **The witness for the seat-key overflow.** A plugin seat key is wider
    /// than the label column `default` sized, and `{:<W$}` pads to a minimum
    /// without cutting — so the key ran straight into the model with no gap
    /// and carried the row past the card's own border.
    ///
    /// The two never abut, and the header's label moves out to the same
    /// column so the card keeps one left edge.
    #[test]
    fn a_seat_key_wider_than_the_label_column_still_keeps_its_gap() {
        let mut state = state();
        state.roles.push(wiring(
            "acme/reviewer",
            "anthropic/claude-opus-5",
            "provider default",
            "seat_models.acme/reviewer",
        ));
        let text = body_of(&WorkspaceModel::new(), &state, false);

        assert!(
            !text.contains("acme/revieweranthropic"),
            "the seat key abuts its model with no gap:\n{text}"
        );
        assert!(
            text.contains("acme/reviewer  anthropic/claude-opus-5"),
            "the seat key and its model are one padded column apart:\n{text}"
        );
        // The header shares the widened column rather than keeping the old
        // one — two left edges in one card is the same defect seen twice.
        assert!(
            text.contains("session        "),
            "the header label did not widen with the table:\n{text}"
        );
        let widest = text.lines().map(columns::width).max().unwrap_or(0);
        assert!(
            widest <= 68,
            "a row ran past the card's inner width ({widest} > 68):\n{text}"
        );
    }

    /// Copy law (D6): the deck's one word labels its row rather than the
    /// settings key underneath it.
    #[test]
    fn the_rows_are_labeled_in_the_decks_own_vocabulary() {
        let text = rendered(&WorkspaceModel::new(), false);
        assert!(
            text.contains("lead"),
            "missing the \"lead\" row in:\n{text}"
        );
        assert!(
            !text.contains(&format!("{:<LABEL_W$}", "default")),
            "the settings key is labeling the row instead of the deck's word:\n{text}"
        );
    }

    /// The bug this dialog would otherwise ship with. The `effort_auto`
    /// disclosure is the longest detail the card can carry, so a truncating
    /// layout drops exactly the sentence the dialog exists for — and does it
    /// on any terminal short of very wide.
    #[test]
    fn a_long_detail_wraps_rather_than_losing_its_disclosure() {
        let text = rendered(&WorkspaceModel::new(), false);
        assert!(text.contains("effort_auto replaced"), "{text}");
        assert!(text.contains("from agents.default.model"), "{text}");
        assert!(!text.contains('…'), "nothing was elided:\n{text}");

        // Every wrapped row stays inside the width it was given.
        let parts = ["a".repeat(30), "b".repeat(30), "c".repeat(10)].map(String::from);
        for row in wrap_parts(&parts, 40) {
            assert!(row.width() <= 40, "{row:?} overflows 40 cols");
        }
        // A part wider than the whole row has nowhere to go but an ellipsis.
        assert_eq!(wrap_parts(&["x".repeat(20)].map(String::from), 8).len(), 1);
    }

    /// The auto-modes are stated, because they are the explanation for an
    /// effort that does not match the pin a user remembers writing.
    #[test]
    fn the_header_states_which_auto_modes_are_deciding() {
        let text = rendered(&WorkspaceModel::new(), false);
        assert!(text.contains("effort on · thinking off"), "{text}");
    }

    /// Accessible mode emits labeled records, not columns: a screen reader
    /// hears which field it is on rather than a position in a grid.
    #[test]
    fn accessible_mode_labels_every_field() {
        let text = rendered(&WorkspaceModel::new(), true);
        assert!(
            text.contains("· lead · model anthropic/claude-opus-5"),
            "{text}"
        );
    }

    /// An unsaved edit in the ENGINE overlay must not be printed here as
    /// though it were in force — the dialog reads the last driver snapshot.
    #[test]
    fn the_dialog_reads_the_driver_snapshot_not_the_overlays_draft() {
        let mut ui = DeckUi::default();
        ui.engine.pristine = Some(state());
        let mut draft = state();
        draft.roles[0].model = "typed-but-never-saved".to_string();
        ui.engine.state = Some(draft);
        let chosen = ui.engine.pristine.as_ref().or(ui.engine.state.as_ref());
        assert_eq!(chosen.unwrap().roles[0].model, "anthropic/claude-opus-5");
    }

    /// **The #3472 witness.** A role the deck has no word for is *printed*,
    /// with its model and the setting that chose it, and disappears when the
    /// driver stops sending it — which is what happens when whatever
    /// contributed it is removed.
    ///
    /// Before this, the dialog looped its own slot list and looked each key
    /// up, so a contributed row was dropped silently: it resolved, ran and
    /// spent, and the one surface that answers "what will each role run" said
    /// nothing about it. The second half is what makes the first safe — a
    /// table that could only ever grow would keep printing a routing answer
    /// for a role that no longer routes anywhere. [`KNOWN`] had grown to that
    /// shape too, naming four roles the driver could never send again;
    /// shrinking it back to the one role the driver can send does not narrow
    /// what this fold renders, only what it has a word for — a future seat
    /// still shows up here under its own key, same as `vera-witness` below.
    #[test]
    fn a_contributed_role_is_printed_and_leaves_with_its_contributor() {
        let model = WorkspaceModel::new();
        let mut installed = state();
        installed.roles.push(wiring(
            "vera-witness",
            "anthropic/claude-opus-5-mini",
            "high",
            "plugin vera",
        ));
        let text = body_of(&model, &installed, false);
        for needle in [
            "vera-witness",
            "anthropic/claude-opus-5-mini",
            "plugin vera",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }

        let text = body_of(&model, &state(), false);
        assert!(
            !text.contains("vera-witness"),
            "a role whose contributor is gone must leave the dialog with it:\n{text}"
        );
    }

    /// The built-in row keeps its word and its place when a contributed role
    /// joins it: the reading order someone has learned is not something a
    /// contribution gets to rearrange.
    #[test]
    fn a_contributed_role_joins_the_end_and_moves_nothing() {
        let mut state = state();
        state
            .roles
            .push(wiring("aaa-contributed", "zai/glm-5.2", "low", "plugin a"));
        let words: Vec<&str> = role_table(&state, &KNOWN)
            .into_iter()
            .map(|entry| entry.word)
            .collect();
        assert_eq!(words, vec!["lead", "aaa-contributed"]);
    }
}
