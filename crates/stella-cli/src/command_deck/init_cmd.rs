// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The Command Deck's `/init` command.
//!
//! Extracted from `command_deck.rs` rather than added to it: that file is a
//! grandfathered god file closed to growth (AGENTS.md § "God files"), and
//! `/init` needed one more channel — the deck's [`AskUserIo`], so the
//! first-session TOML conversion offer can raise a real question card instead
//! of printing a TTY prompt straight through the full-screen render.
//!
//! Behaviour is unchanged from the inline arm apart from that added channel:
//! the launch cinematic still replays over the reindex and is still released
//! on **both** outcomes, because a failed init must never strand a held
//! splash.

use tokio::sync::mpsc::UnboundedSender;

use crate::agent::{self, InitIo};
use crate::interactive::AskUserIo;

/// Run the deck's `/init`: replay the splash over the reindex, drive the
/// shared [`agent::init_workspace`] flow, and release the splash whatever
/// happens.
///
/// Init narrates through the deck's own narrator — a step joins the
/// transcript behind the splash, a counter rewrites the counter before it —
/// and `ask` is the deck's question channel, which init uses only for the one
/// first-session conversion offer. Returns `Ok(())` when init completed, or
/// the error message to surface.
pub(super) async fn run(
    provider: &dyn stella_protocol::Provider,
    workspace_root: &std::path::Path,
    model_id: &str,
    budget_limit: Option<f64>,
    ask: &dyn AskUserIo,
    in_tx: &UnboundedSender<super::Inbound>,
    agent_name: &str,
) -> Result<(), String> {
    let splash = splash_sender(in_tx);
    splash(super::SplashCue::Replay);
    let mut io = InitIo::new(agent::deck_narrator(in_tx.clone(), agent_name), Some(ask));
    let outcome = agent::init_workspace(
        Some(provider),
        workspace_root,
        Some(model_id),
        budget_limit,
        &mut io,
    )
    .await;
    splash(super::SplashCue::Release);
    outcome.map(|_| ())
}

/// Send a splash cue on the deck's inbound channel.
fn splash_sender(in_tx: &UnboundedSender<super::Inbound>) -> impl Fn(super::SplashCue) + '_ {
    move |cue| {
        let _ = in_tx.send(super::Inbound::Splash(cue));
    }
}
