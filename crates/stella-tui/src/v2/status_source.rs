//! The live projection behind [`super::status_bar`] — SPEC 5's six values,
//! read off the deck's own state.
//!
//! [`super::status_bar::Status`] is deliberately pure: six scalars and two
//! borrowed strings, so the widget's golden frames are fixture data all the way
//! down. That purity is what makes this module necessary — something has to own
//! the two `String`s the widget borrows, and it cannot be the widget.
//!
//! Every source here is the one the v1 statline already reads, cell for cell,
//! so the migration changes the *shape* of the row and not a single number in
//! it. Where the two rows disagree it is because SPEC 5 says so, and each of
//! those is called out at its field below.

use super::project::vendor_slug;
use super::status_bar::{CTX_WINDOW, Status};
use crate::WorkspaceModel;
use crate::deck::PipelineRole;
use crate::deck_ui::DeckUi;

/// Owns the two strings [`Status`] borrows, so a caller can project once per
/// frame and lend the widget a `Status` for the draw.
#[derive(Clone, Debug, Default)]
pub struct StatusSource {
    worker: String,
    stage: String,
    ctx_used: f64,
    spend_usd: f64,
    saved_usd: f64,
    inbox: u32,
    deadline_remaining_ms: Option<u64>,
}

impl StatusSource {
    /// Read SPEC 5's six values off the deck's state.
    #[must_use]
    pub fn project(model: &WorkspaceModel, ui: &DeckUi) -> Self {
        let focused = model.agents.get(ui.focused);

        Self {
            worker: worker_slug(model),
            stage: stage_word(focused.and_then(|a| a.model.hud.stage.as_ref())),
            // Window occupancy is the latest call's prompt size, not the
            // session's cumulative input — the same read `ctx_item` makes, and
            // for the same reason: cumulative input dwarfs the window after a
            // few turns and the meter would peg on turn three and stay there.
            ctx_used: focused.map_or(0, |a| a.context_tokens) as f64 / CTX_WINDOW as f64,
            spend_usd: model.total_cost(),
            saved_usd: model.total_cache_savings_usd(),
            // Unread only. The badge exists to be cleared, so a read
            // notification is not something the row should still be counting.
            inbox: ui.notifications.iter().filter(|n| !n.read).count() as u32,
            // The armed task deadline, or nothing. SPEC 5 gives this a cell on
            // the bar only while it is armed, so the `Option` is the whole
            // signal and must not be flattened to a number here.
            deadline_remaining_ms: focused.and_then(|a| a.model.hud.deadline_remaining_ms),
        }
    }

    /// Lend the widget its borrowed view.
    #[must_use]
    pub fn status(&self) -> Status<'_> {
        Status {
            worker: &self.worker,
            stage: &self.stage,
            ctx_used: self.ctx_used,
            spend_usd: self.spend_usd,
            saved_usd: self.saved_usd,
            inbox: self.inbox,
            deadline_remaining_ms: self.deadline_remaining_ms,
        }
    }
}

/// The model actually answering, as `vendor/model` (SPEC 5).
///
/// v1 prints this as `worker: vendor/model` because its label row heads the
/// cell with `MODEL` and the role disambiguates which pin is meant. v2 drops
/// the role word: the row has no labels, and on a one-row bar the leading cell
/// is read as "who is answering" without being told.
///
/// Role preference is v1's, unchanged — the active role if it is pinned, else
/// the worker, else whatever is pinned at all — because the question ("which
/// pin is serving right now") did not change with the row's shape.
fn worker_slug(model: &WorkspaceModel) -> String {
    model
        .active_role
        .filter(|r| model.role_pins.contains_key(r))
        .or_else(|| {
            PipelineRole::ORDER
                .iter()
                .copied()
                .find(|r| *r == PipelineRole::Worker && model.role_pins.contains_key(r))
        })
        .or_else(|| {
            PipelineRole::ORDER
                .iter()
                .copied()
                .find(|r| model.role_pins.contains_key(r))
        })
        .and_then(|role| model.role_pins.get(&role))
        .map(vendor_slug)
        .unwrap_or_else(|| "—".into())
}

/// The stage as SPEC 5 writes it: one lowercase word.
///
/// v1 uppercases the same value because there it *is* the label row's heading
/// and every heading on that row is chrome. Here the stage is a value among
/// values, so it takes the row's own case — `execute`, not `EXECUTE`.
/// A plugin's contributed stage is passed through in the plugin's own word,
/// lowercased for the same reason (the stage vocabulary is open, #3964).
fn stage_word(stage: Option<&stella_protocol::StageName>) -> String {
    use stella_protocol::StageKind as S;
    let Some(stage) = stage else {
        return "idle".into();
    };
    let Some(kind) = stage.kind() else {
        return stage.as_str().to_lowercase();
    };
    match kind {
        S::Triage => "triage",
        S::ContextRecall => "context recall",
        S::Research => "research",
        S::Plan => "plan",
        S::ScopeReview => "scope review",
        S::Witness => "witness",
        S::Execute => "execute",
        S::Verify => "verify",
        S::Verdict => "verdict",
        S::Reflect => "reflect",
        S::ContextWrite => "context write",
        S::Complete => "complete",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No pins at all is the boot state, and the bar must still draw.
    #[test]
    fn an_unpinned_deck_reads_em_dash_not_empty() {
        let model = WorkspaceModel::default();
        assert_eq!(worker_slug(&model), "—");
    }

    /// The stage is a value on this row, not a heading — SPEC 5.
    #[test]
    fn the_stage_word_is_lowercase_where_v1_uppercases_it() {
        let stage = stella_protocol::StageName::from(stella_protocol::StageKind::Execute);
        assert_eq!(stage_word(Some(&stage)), "execute");
    }

    /// No stage observed yet still names a state.
    #[test]
    fn no_stage_reads_idle() {
        assert_eq!(stage_word(None), "idle");
    }

    /// A plugin's own stage word survives, lowercased (#3964).
    #[test]
    fn a_contributed_stage_keeps_its_own_word() {
        let stage = stella_protocol::StageName::new("Vera Verify");
        assert_eq!(stage_word(Some(&stage)), "vera verify");
    }

    /// The default deck projects a drawable status: no NaN into the meter.
    #[test]
    fn a_default_deck_projects_a_finite_meter() {
        let source = StatusSource::project(&WorkspaceModel::default(), &DeckUi::default());
        assert!(source.status().ctx_used.is_finite());
    }
}
