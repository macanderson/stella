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
//!
//! Nothing here reads a clock, the environment or the process table. The bar is
//! a projection of the fold and the frame is a function of the bar, which is
//! what makes a v2 frame reproducible from a scenario.
//!
//! # Why this is the only projection
//!
//! There were two. `src/v2/project.rs` held a second reading of the same six
//! values — borrowing from the model rather than owning, and rendering the
//! stage as [`stella_protocol::StageName::as_str`], which is the **wire**
//! string. It was the one the deck actually drew, so the shipping bar printed
//! `context_recall` and `scope_review` at a human where SPEC 5 says
//! `context recall` and `scope review`, while the module that spelled them
//! correctly was called by nothing but its own tests (#4187).
//!
//! This one survived rather than that one because the fix settles the argument
//! between them: a stage word SPEC 5 can print is *computed*, not borrowed, so
//! there is a `String` to own either way — a contributed stage's word is
//! lowercased at projection time (#3964), which a borrowing projection has
//! nowhere to put — and owning it here beats handing the draw path two
//! out-of-band strings to compute in the right order. The other module is
//! deleted rather than synced: two answers to one question is the defect, and
//! keeping both is how one of them goes stale again. What did not survive is
//! that module's `idle` for an unannounced stage; `stage_word` below carries
//! the reason.

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
            stage: stage_cell(model.now_ms, focused),
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

/// A pin's slug with the API gateway stripped: the *model's own* vendor, not
/// whoever proxied the call.
///
/// [`crate::deck::RolePin::slug`] answers "where did this call go", which is the
/// right question for routing and the wrong one for a status bar — a reader
/// wants to know which model is thinking, not which reseller billed it.
fn vendor_slug(pin: &crate::deck::RolePin) -> String {
    if pin.model.contains('/') || pin.provider.is_empty() {
        pin.model.clone()
    } else {
        format!("{}/{}", pin.provider, pin.model)
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

/// The stage cell: the stage word, or — while the turn is parked on a wait
/// (#2007) — `parked ⏳ 0:10 / 30:00`, elapsed against the deadline.
///
/// The park rode the v1 stage box, which the v2 frame deleted; a parked turn
/// and a working one must still read differently on every frame, and the
/// stage cell is the one that says what the run is doing right now. The
/// elapsed comes from `now_ms` on the model rather than a clock, so the bar
/// stays a pure projection.
fn stage_cell(now_ms: u64, focused: Option<&crate::deck::AgentEntry>) -> String {
    if let Some((park, elapsed_ms)) = focused.and_then(|a| a.live_park(now_ms)) {
        return format!(
            "parked ⏳ {} / {}",
            clock_ms(elapsed_ms),
            clock_ms(park.deadline_secs.saturating_mul(1000))
        );
    }
    stage_word(focused.and_then(|a| a.model.hud.stage.as_ref()))
}

/// `M:SS`, rolling to `H:MM:SS` past an hour — both halves of the park clock
/// render through this so elapsed and deadline are always comparable.
fn clock_ms(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// The stage as SPEC 5 writes it: one lowercase word.
///
/// v1 uppercases the same value because there it *is* the label row's heading
/// and every heading on that row is chrome. Here the stage is a value among
/// values, so it takes the row's own case — `execute`, not `EXECUTE`.
/// A plugin's contributed stage is passed through in the plugin's own word,
/// lowercased for the same reason (the stage vocabulary is open, #3964).
///
/// An unannounced stage reads `—`, never a word. `idle` is a claim about the
/// run and it is routinely false: a plain `stella run` is the raw step-loop and
/// emits no stage boundaries at all (AGENTS.md's opening), so the bar would
/// call a working turn idle for its whole length. A bar that guesses where the
/// run is has spent the credibility the whole row exists to hold.
fn stage_word(stage: Option<&stella_protocol::StageName>) -> String {
    use stella_protocol::StageKind as S;
    let Some(stage) = stage else {
        return "—".into();
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

    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use stella_protocol::{AgentEvent, StageKind, StageScope};

    use crate::envelope::{AgentMeta, Inbound};

    /// The row `render_band` actually paints, as text.
    fn live_bar(kind: StageKind) -> String {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Stage {
                name: kind.into(),
                scope: StageScope::Run,
            },
        });
        let area = Rect::new(0, 0, 100, 1);
        let mut buf = Buffer::empty(area);
        super::super::status_bar::render_band(&model, &DeckUi::default(), area, &mut buf);
        (0..area.width)
            .map(|x| buf.cell((x, 0)).map_or(" ", ratatui::buffer::Cell::symbol))
            .collect()
    }

    /// The bar draws SPEC 5's words, never the bytes the stage travels as.
    ///
    /// Drives `render_band` — the function `render_deck` calls — rather than
    /// [`stage_word`] alone, because the defect #4187 records was never a wrong
    /// mapping. It was a correct mapping that nothing on the draw path called,
    /// and only a test that renders can tell those two apart.
    #[test]
    fn the_live_bar_never_prints_a_wire_string_at_a_human() {
        for (kind, word) in [
            (StageKind::ContextRecall, "context recall"),
            (StageKind::ScopeReview, "scope review"),
            (StageKind::ContextWrite, "context write"),
        ] {
            let row = live_bar(kind);
            assert!(
                row.contains(word),
                "the bar must say {word:?}, SPEC 5's word:\n{row}"
            );
            assert!(
                !row.contains(kind.as_wire_str()),
                "the bar leaked the wire string {:?}:\n{row}",
                kind.as_wire_str()
            );
        }
    }

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

    /// No stage observed is the one case the bar must not put a word to.
    #[test]
    fn no_stage_reads_em_dash_rather_than_guessing_idle() {
        assert_eq!(stage_word(None), "—");
    }

    /// A plugin's own stage word survives, lowercased (#3964).
    #[test]
    fn a_contributed_stage_keeps_its_own_word() {
        let stage = stella_protocol::StageName::new("Vera Verify");
        assert_eq!(stage_word(Some(&stage)), "vera verify");
    }

    /// #2007's rendering half, re-homed from the deleted stage box: a parked
    /// turn states elapsed against its deadline, and the cell moves between
    /// two frames with no event in between.
    #[test]
    fn a_parked_turn_counts_up_against_its_deadline_on_the_stage_cell() {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        model.now_ms = 1_000;
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::TurnParked {
                description: "CI for branch main settles".into(),
                poll_interval_secs: 30,
                deadline_secs: 1_800,
            },
        });
        model.now_ms = 11_000;
        let early = StatusSource::project(&model, &DeckUi::default()).stage;
        assert_eq!(early, "parked ⏳ 0:10 / 30:00");
        model.now_ms = 1_753_000;
        let late = StatusSource::project(&model, &DeckUi::default()).stage;
        assert_eq!(late, "parked ⏳ 29:12 / 30:00");
        model.now_ms = 4_401_000;
        let hour = StatusSource::project(&model, &DeckUi::default()).stage;
        assert_eq!(hour, "parked ⏳ 1:13:20 / 30:00");
    }

    /// The default deck projects a drawable status: no NaN into the meter.
    #[test]
    fn a_default_deck_projects_a_finite_meter() {
        let source = StatusSource::project(&WorkspaceModel::default(), &DeckUi::default());
        assert!(source.status().ctx_used.is_finite());
    }
}
