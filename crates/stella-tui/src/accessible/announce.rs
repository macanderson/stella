//! Saying where the session now is.
//!
//! On the alternate screen a tab switch or an overlay is self-evident: the
//! whole grid changes and an eye lands on the new thing. On the normal screen,
//! read aloud, the same transition is **silent** — the pane repaints in place
//! and nothing announces that `⇥` moved from SESSION to AGENTS, that `⌃S`
//! opened the STATE overlay, or that focus moved to another agent's lane. The
//! user is somewhere else and was not told.
//!
//! So accessible mode emits a line into scrollback for each move. The
//! implementation is deliberately a **diff of where-we-are**, not a call at
//! every mutation site: `ui.tab` is written from a dozen places (`⇥`, a slash
//! command, a `/files` jump, an overlay's Enter), and instrumenting each one
//! is how a surface ends up announcing four of them. Snapshot before,
//! snapshot after, say what changed.
//!
//! Everything here rides `Scrollback::announce`, which is inert unless the
//! inline viewport was obtained — with no scrollback there is nowhere for an
//! announcement to go, and the degrade notice has already said so.

use crate::accessible::NOTICE_MARKER;
use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;

/// The modal overlays worth naming, most-recently-claimed first.
///
/// Order matters only for the report, not for correctness: at most one of
/// these is normally up, and if two ever are the outer one is named. Non-modal
/// chrome (the slash menu, the composer's own popups) is deliberately absent —
/// it opens and closes on almost every keystroke, and announcing it would bury
/// the moves that matter.
fn overlay(ui: &DeckUi) -> Option<&'static str> {
    let candidates = [
        (ui.help_open, "help"),
        (ui.queue_open, "queue editor"),
        (ui.sessions_open, "sessions"),
        (ui.context_open, "context"),
        (ui.inbox_open, "inbox"),
        (ui.inspect_open, "inspect"),
        (ui.search.open, "transcript search"),
        (ui.graph_picker_open, "graph file picker"),
    ];
    candidates
        .into_iter()
        .find_map(|(open, name)| open.then_some(name))
}

/// Where the deck is, in the terms a reader needs said out loud.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Locus {
    tab: Option<&'static str>,
    overlay: Option<&'static str>,
    /// The focused lane's id, not its index: an index moving because a new
    /// agent registered ahead of it is not the user going anywhere.
    lane: Option<String>,
}

impl Locus {
    pub(crate) fn of(model: &WorkspaceModel, ui: &DeckUi) -> Self {
        Self {
            tab: Some(ui.tab.title()),
            overlay: overlay(ui),
            lane: model.agents.get(ui.focused).map(|a| a.meta.id.clone()),
        }
    }

    /// The lines to say when the session moves from `self` to `next`.
    ///
    /// A move that changes two things at once — Esc closing an overlay while
    /// something else jumps tabs — says both, in the order a reader would want
    /// them: what closed, then where they are.
    pub(crate) fn transitions(&self, next: &Self) -> Vec<String> {
        let mut out = Vec::new();
        if self.overlay != next.overlay {
            if let Some(closed) = self.overlay {
                out.push(format!("{NOTICE_MARKER}{closed} closed"));
            }
            if let Some(opened) = next.overlay {
                out.push(format!("{NOTICE_MARKER}{opened} opened"));
            }
        }
        if self.tab != next.tab
            && let Some(tab) = next.tab
        {
            out.push(format!("{NOTICE_MARKER}{tab} tab"));
        }
        if self.lane != next.lane
            && let Some(lane) = &next.lane
        {
            out.push(format!("{NOTICE_MARKER}focus {lane}"));
        }
        out
    }
}

/// Run `act`, then announce whatever moved.
///
/// The one entry point, wrapped around the deck's two pure mutators
/// (`handle_deck_key`, `ingest_inbound`). Costs a `Locus` build per call in
/// accessible mode and nothing at all otherwise.
pub(crate) fn announcing<T>(
    model: &WorkspaceModel,
    ui: &mut DeckUi,
    act: impl FnOnce(&mut DeckUi) -> T,
) -> T {
    if !ui.accessible {
        return act(ui);
    }
    let before = Locus::of(model, ui);
    let out = act(ui);
    let after = Locus::of(model, ui);
    for line in before.transitions(&after) {
        ui.scrollback.announce(line);
    }
    out
}

/// [`announcing`] for a mutator that folds the model too (`ingest_inbound`).
///
/// The "before" snapshot is taken against the PRE-fold model deliberately: the
/// locus is where the *user* is, and `ui.focused` is an index into the lane
/// list they were looking at when the envelope arrived.
pub(crate) fn announcing_fold<T>(
    model: &mut WorkspaceModel,
    ui: &mut DeckUi,
    act: impl FnOnce(&mut WorkspaceModel, &mut DeckUi) -> T,
) -> T {
    if !ui.accessible {
        return act(model, ui);
    }
    let before = Locus::of(model, ui);
    let out = act(model, ui);
    for line in before.transitions(&Locus::of(model, ui)) {
        ui.scrollback.announce(line);
    }
    out
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::deck::DeckTab;
    use crate::envelope::{AgentMeta, Inbound};

    fn model_with(ids: &[&str]) -> WorkspaceModel {
        let mut model = WorkspaceModel::new();
        for id in ids {
            model.apply_inbound(&Inbound::Register(AgentMeta::new(*id, "goal", 0)));
        }
        model
    }

    fn ui() -> DeckUi {
        let mut ui = DeckUi::default();
        ui.accessible = true;
        ui.scrollback.set_live(true);
        ui
    }

    #[test]
    fn a_tab_change_is_said_out_loud() {
        let model = model_with(&["lead"]);
        let mut ui = ui();
        announcing(&model, &mut ui, |ui| ui.tab = DeckTab::Agents);
        assert_eq!(ui.scrollback.take_announcements(), vec!["▸ AGENTS tab"]);
    }

    #[test]
    fn an_overlay_says_both_that_it_opened_and_that_it_closed() {
        let model = model_with(&["lead"]);
        let mut ui = ui();
        announcing(&model, &mut ui, |ui| ui.help_open = true);
        assert_eq!(ui.scrollback.take_announcements(), vec!["▸ help opened"]);
        announcing(&model, &mut ui, |ui| ui.help_open = false);
        assert_eq!(ui.scrollback.take_announcements(), vec!["▸ help closed"]);
    }

    #[test]
    fn a_lane_change_names_the_lane_it_moved_to() {
        let model = model_with(&["lead", "sub"]);
        let mut ui = ui();
        announcing(&model, &mut ui, |ui| ui.focus_agent(1));
        assert_eq!(ui.scrollback.take_announcements(), vec!["▸ focus sub"]);
    }

    #[test]
    fn standing_still_says_nothing() {
        // The deck repaints ~30 times a second and every keystroke runs
        // through here. Anything that announces without a move would make the
        // surface unusable, which is worse than announcing nothing.
        let model = model_with(&["lead"]);
        let mut ui = ui();
        announcing(&model, &mut ui, |ui| ui.composer.insert_char('x'));
        assert!(ui.scrollback.take_announcements().is_empty());
    }

    #[test]
    fn two_moves_at_once_are_both_reported_closing_first() {
        let model = model_with(&["lead"]);
        let mut ui = ui();
        announcing(&model, &mut ui, |ui| ui.help_open = true);
        ui.scrollback.take_announcements();
        announcing(&model, &mut ui, |ui| {
            ui.help_open = false;
            ui.tab = DeckTab::Files;
        });
        assert_eq!(
            ui.scrollback.take_announcements(),
            vec!["▸ help closed", "▸ FILES tab"]
        );
    }

    #[test]
    fn an_ordinary_session_is_never_charged_for_this() {
        let model = model_with(&["lead"]);
        let mut ui = DeckUi::default();
        ui.scrollback.set_live(true);
        announcing(&model, &mut ui, |ui| ui.tab = DeckTab::Agents);
        assert!(
            ui.scrollback.take_announcements().is_empty(),
            "announcements are an accessible-mode affordance, not a global one"
        );
    }
}
