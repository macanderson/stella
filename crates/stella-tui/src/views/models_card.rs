//! The model-routing dialog (`/models`): every role's **resolved** wiring —
//! the model it will send, the effort and thinking riding with it, and the
//! setting that decided each.
//!
//! # What this replaced, and why
//!
//! Two things, and the second is the point.
//!
//! It replaced a three-slot card that printed `think` / `work` / `verify` and
//! one `provider/model` slug apiece. A slug alone cannot answer what anybody
//! actually opens this for: *why* is the verifier on that model, is the
//! `effort` I pinned still in force, which of four settings keys do I edit to
//! change it. Those were one-inference-from-silence questions, and this
//! dialog exists to make them readable instead.
//!
//! It also replaced the statline's permanent `T·… W·… J·…` row. Routing is a
//! thing you *check*, not a glanceable: the pins do not move during a session,
//! so a row that never changed was paying rent on every frame. The statline
//! keeps naming the pin that is actually answering (its MODEL cell), and
//! nothing more.
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
//! # Copy law (D6)
//!
//! The three pipeline slots keep the deck's own words — `think` / `work` /
//! `verify` — and the interactive agent is `lead`, the word the deck already
//! uses for that lane. The internal role identifiers reach the rendered
//! surface only inside a `from` path (`agents.triage.model`), where they are
//! not a name for the slot but the literal key a user would edit.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::deck::{PipelineRole, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::envelope::{EngineConfigState, RoleWiringRow};
use crate::theme;
use crate::views::cards;

/// Wider than [`cards::CARD_MAX_W`], the only card that is: a model slug and
/// the settings key that chose it are fixed strings that lose their meaning
/// elided, and `openrouter/moonshotai/kimi-k3` alone would spend 29 of the
/// standard card's 52 usable columns.
const MODELS_CARD_W: u16 = 72;

/// Width of the dimmed left label column — the longest role word plus room.
const LABEL_W: usize = 10;

/// Columns held back from the model slug so a standing word has somewhere to
/// land on the same row.
const STANDING_W: usize = 12;

/// The roles this dialog prints, in pipeline order with the interactive lane
/// first: the settings key the driver sends each row under, and the word the
/// deck says out loud for it.
const SLOTS: [(&str, &str); 4] = [
    ("default", "lead"),
    ("triage", "think"),
    ("worker", "work"),
    ("verifier", "verify"),
];

/// Which pipeline slot a settings role key belongs to. `default` is the
/// interactive lane and belongs to none — it is not one of the three a scored
/// run is read against.
fn slot_of(role_key: &str) -> Option<PipelineRole> {
    match role_key {
        "triage" => Some(PipelineRole::Triage),
        "worker" => Some(PipelineRole::Worker),
        "verifier" => Some(PipelineRole::Verifier),
        _ => None,
    }
}

/// Whether this role has actually served a call, as one right-aligned word.
///
/// The distinction the fold keeps deliberately: `configured` is a claim about
/// intent and `served` is evidence, and a scored run is read against the
/// second. A slot the session never reached must not read like one that ran.
fn standing(model: &WorkspaceModel, role_key: &str) -> Option<&'static str> {
    let slot = slot_of(role_key)?;
    Some(match model.role_pins.get(&slot) {
        Some(pin) if pin.served => "served",
        Some(_) => "configured",
        None => "unassigned",
    })
}

/// Pack `parts` onto as few `width`-column rows as they fit on, joined by the
/// deck's ` · ` separator.
///
/// Wrapped rather than truncated because of what these parts are: the longest
/// one is `high  (effort_auto replaced "max")`, and an ellipsis lands squarely
/// on the disclosure — the single most useful thing this dialog can say — the
/// moment the terminal is anything short of very wide. A part that cannot fit
/// a row on its own still gets elided; nothing else can be done with it.
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
    model: &WorkspaceModel,
    row: &RoleWiringRow,
    word: &str,
    inner_w: usize,
    accessible: bool,
) -> Vec<Line<'static>> {
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let parts = [
        row.effort.clone(),
        row.thinking.clone(),
        format!("from {}", row.source),
    ];
    let detail = parts.join(" · ");
    let mark = standing(model, &row.role);

    if accessible {
        // A labeled record per role: no column alignment to hear, and every
        // field named rather than positional.
        let mut text = format!("· {word} · model {} · {detail}", row.model);
        if let Some(mark) = mark {
            text.push_str(" · ");
            text.push_str(mark);
        }
        return vec![Line::from(Span::styled(
            text,
            Style::new().fg(theme::TEXT_PRIMARY),
        ))];
    }

    // The role that most recently served is the one answering right now, and
    // it carries the accent on every surface that names a pin.
    let active = slot_of(&row.role).is_some_and(|slot| model.active_role == Some(slot));
    let model_style = if active {
        theme::accent().add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(theme::TEXT_PRIMARY)
    };
    let mut head = vec![
        Span::styled(format!("{word:<LABEL_W$}"), dim),
        Span::styled(
            cards::truncate_cols(&row.model, inner_w.saturating_sub(LABEL_W + STANDING_W)),
            model_style,
        ),
    ];
    if let Some(mark) = mark {
        head = cards::pad_right(head, Span::styled(mark.to_string(), dim), inner_w);
    }
    let mut rows = vec![Line::from(head)];
    for line in wrap_parts(&parts, inner_w.saturating_sub(LABEL_W)) {
        rows.push(Line::from(vec![
            Span::raw(" ".repeat(LABEL_W)),
            Span::styled(line, dim),
        ]));
    }
    rows
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
    accessible: bool,
) -> Vec<Line<'static>> {
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let on_off = |on: bool| if on { "on" } else { "off" };
    let autos = format!(
        "model {} · effort {} · thinking {}",
        on_off(state.auto_mode),
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
                Style::new().fg(theme::TEXT_PRIMARY),
            ))
        } else {
            Line::from(vec![
                Span::styled(format!("{label:<LABEL_W$}"), dim),
                Span::styled(value, Style::new().fg(theme::TEXT_PRIMARY)),
            ])
        }
    };
    vec![row("session", session), row("auto", autos), Line::default()]
}

/// Render the model-routing dialog over `frame`.
pub fn render(model: &WorkspaceModel, ui: &DeckUi, frame: Rect, buf: &mut Buffer) {
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let w = frame.width.min(MODELS_CARD_W);
    let inner_w = w.saturating_sub(4).max(LABEL_W as u16 + 8) as usize;

    // `pristine` is the last driver snapshot adopted verbatim, never the
    // ENGINE overlay's working copy — so an unsaved edit being typed one tab
    // over can never be printed here as though it were in force.
    let state = ui.engine.pristine.as_ref().or(ui.engine.state.as_ref());

    let mut rows: Vec<Line<'static>> = Vec::new();
    match state.filter(|s| !s.roles.is_empty()) {
        // Honest rather than convenient: the driver seeds this at startup, so
        // an empty snapshot means the answer is genuinely not in yet.
        None => rows.push(Line::from(Span::styled(
            "routing has not been resolved yet",
            dim,
        ))),
        Some(state) => {
            rows.extend(header_rows(model, state, ui.accessible));
            for (key, word) in SLOTS {
                if let Some(row) = state.roles.iter().find(|r| r.role == key) {
                    rows.extend(role_rows(model, row, word, inner_w, ui.accessible));
                }
            }
        }
    }

    let area = cards::card_area(frame, rows.len() as u16, MODELS_CARD_W, ui.accessible);
    let inner = cards::card_frame(
        area,
        "models",
        vec![Span::styled("resolved routing", dim)],
        "esc close",
        buf,
    );
    cards::render_body(rows, None, inner, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::RolePin;

    fn wiring(role: &str, model: &str, effort: &str, source: &str) -> RoleWiringRow {
        RoleWiringRow {
            role: role.to_string(),
            model: model.to_string(),
            effort: effort.to_string(),
            thinking: "thinking on".to_string(),
            source: source.to_string(),
        }
    }

    fn state() -> EngineConfigState {
        EngineConfigState {
            effort_auto: true,
            roles: vec![
                wiring("default", "zai/glm-5.2", "medium", "default_model"),
                wiring("worker", "zai/glm-5.2", "medium", "default_model"),
                wiring(
                    "verifier",
                    "anthropic/claude-opus-5",
                    "high  (effort_auto replaced \"max\")",
                    "pipeline_verifier_model",
                ),
                wiring("triage", "z-ai/glm-5.2", "low", "agents.triage.model"),
            ],
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

    fn rendered(model: &WorkspaceModel, accessible: bool) -> String {
        let state = state();
        let mut lines = header_rows(model, &state, accessible);
        for (key, word) in SLOTS {
            let row = state.roles.iter().find(|r| r.role == key).unwrap();
            lines.extend(role_rows(model, row, word, 68, accessible));
        }
        text_of(&lines)
    }

    /// The dialog's whole reason to exist: every role names the setting that
    /// chose its model, and an effort names what an auto-mode took from it. A
    /// slug alone is what the card this replaced already showed.
    #[test]
    fn every_role_names_its_model_its_effort_and_the_setting_that_chose_it() {
        let text = rendered(&WorkspaceModel::new(), false);
        for needle in [
            "pipeline_verifier_model",
            "agents.triage.model",
            "default_model",
            "effort_auto replaced",
            "anthropic/claude-opus-5",
            "thinking on",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
    }

    /// Copy law (D6): the deck's words label the rows. The internal role
    /// identifiers appear only inside a settings path, where they are the key
    /// a user edits rather than a name for the slot.
    #[test]
    fn the_rows_are_labeled_in_the_decks_own_vocabulary() {
        let text = rendered(&WorkspaceModel::new(), false);
        for word in ["lead", "think", "work", "verify"] {
            assert!(text.contains(word), "missing the {word:?} row in:\n{text}");
        }
        // `triage` survives only inside `agents.triage.model` — never in the
        // label column, which is what the padding below would prove.
        assert!(
            !text.contains(&format!("{:<LABEL_W$}", "triage")),
            "an internal role identifier is labeling a row:\n{text}"
        );
    }

    /// The bug this dialog would otherwise ship with. `verify`'s detail is
    /// the longest line on the card and carries the `effort_auto` disclosure
    /// at its far end, so a truncating layout drops exactly the sentence the
    /// dialog exists for — and does it on any terminal short of very wide.
    #[test]
    fn a_long_detail_wraps_rather_than_losing_its_disclosure() {
        let text = rendered(&WorkspaceModel::new(), false);
        assert!(text.contains("effort_auto replaced"), "{text}");
        assert!(text.contains("from pipeline_verifier_model"), "{text}");
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
        assert!(
            text.contains("model off · effort on · thinking off"),
            "{text}"
        );
    }

    /// Intent must never read as evidence. A verifier that was configured and
    /// never reached says so, and only a role that actually served is marked
    /// `served` — the same distinction the fold keeps.
    #[test]
    fn a_configured_role_is_not_drawn_as_one_that_ran() {
        let mut model = WorkspaceModel::new();
        model.role_pins.insert(
            PipelineRole::Worker,
            RolePin {
                provider: "zai".into(),
                model: "glm-5.2".into(),
                served: true,
            },
        );
        model.role_pins.insert(
            PipelineRole::Verifier,
            RolePin {
                provider: "anthropic".into(),
                model: "claude-opus-5".into(),
                served: false,
            },
        );
        assert_eq!(standing(&model, "worker"), Some("served"));
        assert_eq!(standing(&model, "verifier"), Some("configured"));
        assert_eq!(standing(&model, "triage"), Some("unassigned"));
        // The interactive lane is not one of the three a scored run is read
        // against, so it claims no standing at all.
        assert_eq!(standing(&model, "default"), None);
    }

    /// Accessible mode emits labeled records, not columns: a screen reader
    /// hears which field it is on rather than a position in a grid.
    #[test]
    fn accessible_mode_labels_every_field() {
        let text = rendered(&WorkspaceModel::new(), true);
        assert!(
            text.contains("· verify · model anthropic/claude-opus-5"),
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
        assert_eq!(chosen.unwrap().roles[0].model, "zai/glm-5.2");
    }
}
