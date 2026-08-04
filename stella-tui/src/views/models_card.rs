//! The model-routing card (`/models`): the three labeled slots — `think` /
//! `work` / `verify` — that replaced the statline's permanent MODELS row.
//! Routing is a thing you check, not a glanceable: it lives here and on the
//! scope card, never in permanent chrome.
//!
//! Each slot renders its pin's `provider/model` slug with its standing:
//! `served` once a call has actually run on it (evidence), `configured`
//! while it is only intent — the distinction the fold keeps deliberately,
//! so a slot that never ran cannot read as one that did.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::deck::{PipelineRole, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::theme;
use crate::views::cards;

/// The rendered label per slot — this surface's own vocabulary (the internal
/// role identifiers never reach a rendered string).
fn slot_label(role: PipelineRole) -> &'static str {
    match role {
        PipelineRole::Triage => "think",
        PipelineRole::Worker => "work",
        PipelineRole::Judge => "verify",
    }
}

/// Render the model-routing card over `frame`.
pub fn render(model: &WorkspaceModel, ui: &DeckUi, frame: Rect, buf: &mut Buffer) {
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let mut rows: Vec<Line<'static>> = Vec::new();
    for role in PipelineRole::ORDER {
        let label = slot_label(role);
        let active = model.active_role == Some(role) && model.active_count() > 0;
        let (slug, standing) = match model.role_pins.get(&role) {
            Some(pin) => (pin.slug(), if pin.served { "served" } else { "configured" }),
            None => ("—".to_string(), "unassigned"),
        };
        if ui.accessible {
            rows.push(Line::from(Span::styled(
                format!("· slot {label} · model {slug} · standing {standing}"),
                Style::new().fg(theme::TEXT_PRIMARY),
            )));
            continue;
        }
        let style = if active {
            theme::accent()
        } else if standing == "served" {
            Style::new().fg(theme::TEXT_PRIMARY)
        } else {
            dim
        };
        let mut row = vec![
            Span::styled(format!("{label:<8}"), dim),
            Span::styled(slug, style),
        ];
        row = cards::pad_right(
            row,
            Span::styled(standing.to_string(), dim),
            (cards::CARD_MAX_W - 2) as usize,
        );
        rows.push(Line::from(row));
    }
    let area = cards::card_area(frame, rows.len() as u16, cards::CARD_MAX_W, ui.accessible);
    let inner = cards::card_frame(
        area,
        "models",
        vec![Span::styled("think · work · verify", dim)],
        "esc close",
        buf,
    );
    cards::render_body(rows, None, inner, buf);
}
