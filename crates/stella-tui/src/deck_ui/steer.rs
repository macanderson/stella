// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What Esc delivers into a running turn.
//!
//! Split out of [`super`] (a god file, closed to growth) rather than grown
//! into it. Small on purpose: the *decision* — steer or stop — belongs with
//! the rest of the Esc precedence rules where a reader can see it beside every
//! other Esc context; only the drain of what to send lives here.

use super::{AgentControl, AgentId, DeckAction, DeckUi, WorkspaceInput, WorkspaceModel};

/// What the FIRST Esc does at a running agent.
///
/// With something to say — a draft in the composer, prompts in the queue, or
/// both — it **steers**: the texts go into the running turn at its next step
/// boundary and the turn keeps going. With nothing to say it is a plain
/// interrupt, because with no guidance to deliver that is the only thing the
/// keystroke can mean. The double-Esc full stop is unaffected either way, so
/// "stop now" stays one press further.
///
/// [`crate::envelope::steering`] carries why this is not a cancel.
pub(super) fn first_esc(ui: &mut DeckUi, model: &WorkspaceModel, agent: AgentId) -> DeckAction {
    let texts = steering_texts(ui, model);
    if texts.is_empty() {
        return DeckAction::Send(WorkspaceInput::Control {
            agent,
            control: AgentControl::Stop,
        });
    }
    DeckAction::Send(WorkspaceInput::Steer { agent, texts })
}

/// Everything the user has waiting to say, in the order they said it: the
/// prompt queue's backlog first, then the composer's current draft.
///
/// Drains both — the composer is cleared and the queue is emptied — because
/// this is the *delivery*, not a preview of one. Leaving either in place would
/// mean the same words are dispatched twice: once as steering now, once as the
/// next turn later.
///
/// The composer goes last for the reason a queue is a queue: the draft is the
/// newest thing typed, and a correction that arrives after two earlier ones
/// should be read after them. It is included at all because a user who has
/// just typed a sentence and pressed Esc plainly meant *that* sentence — the
/// old behaviour left it sitting in the composer while the turn it was written
/// for was cancelled out from under it.
///
/// A `>`-prefixed draft is stripped of its marker: it already says "steer",
/// and this whole path is steering.
fn steering_texts(ui: &mut DeckUi, model: &WorkspaceModel) -> Vec<String> {
    let mut texts: Vec<String> = model
        .queue
        .items
        .iter()
        .map(|p| {
            p.text
                .trim_start()
                .trim_start_matches('>')
                .trim()
                .to_string()
        })
        .filter(|t| !t.is_empty())
        .collect();
    if !ui.composer.is_blank() {
        let draft = ui.composer.buffer().trim().to_string();
        let draft = draft.trim_start_matches('>').trim().to_string();
        if !draft.is_empty() {
            texts.push(draft);
        }
    }
    if !texts.is_empty() {
        ui.composer.clear();
    }
    texts
}
