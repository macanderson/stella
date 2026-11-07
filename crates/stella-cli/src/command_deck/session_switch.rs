//! What a deck session switch moves to the session it adopts: the journal,
//! the durability binding, and the `SessionStart` hook text. Split out of
//! `command_deck.rs`, which is closed to growth.

use std::path::Path;

use tokio::sync::mpsc::UnboundedSender;

use super::session_override::{HookPlane, PromptPlane, adopt_session_hook_context};
use super::{Inbound, chrome_note};
use crate::config::Config;
use crate::session_persist::SharedSink;

/// Point the journal, the durability binding and the hook text at
/// `session_id`. A store that will not open is a note on the deck. The switch
/// has already happened, and the session runs on without that store.
///
/// The order is fixed. The hook payload names the session the bind sets, so
/// the hooks run after it. The caller restores the adopted chat once this
/// returns, because that restore seeds the prompt rebuilt here.
pub(super) async fn rebind(
    cfg: &Config,
    session_id: &str,
    sidecar_dir: &Path,
    journal_sink: &SharedSink,
    deck_tx: &UnboundedSender<Inbound>,
    prompts: PromptPlane<'_>,
    hooks: HookPlane<'_>,
) {
    {
        let mut sink = journal_sink.lock().unwrap_or_else(|p| p.into_inner());
        match stella_store::journal::SessionJournal::open(sidecar_dir) {
            Ok(j) => sink.swap(Some(j)),
            Err(e) => {
                sink.swap(None);
                let _ = deck_tx.send(chrome_note(format!(
                    "session journaling unavailable — this session will not be resumable ({e})"
                )));
            }
        }
    }
    // The next turn's engine reads the binding again, so its resume point
    // lands in the session the user just switched to. The session left behind
    // keeps its resume point and commits as they stood.
    if let Some(warning) =
        crate::durability::bind_session(&cfg.durability, &cfg.workspace_root, session_id)
    {
        let _ = deck_tx.send(chrome_note(warning));
    }
    adopt_session_hook_context(cfg, prompts, hooks).await;
}
