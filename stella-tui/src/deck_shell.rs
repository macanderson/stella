//! The deck's async run loop — the multi-agent analogue of [`crate::shell::run`].
//!
//! Folds the [`Inbound`] stream, handles keys via [`crate::deck_ui`], ticks
//! animations + resource sampling on a fixed cadence, and redraws via
//! [`crate::deck_render`]. STUB: the terminal guard, reader thread, and
//! `tokio::select` loop are filled in by the deck builder; the signature here
//! is the frozen wiring seam a caller connects the engine to.

use std::io;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::envelope::{Inbound, WorkspaceInput};

/// Configuration for one deck session.
#[derive(Debug, Clone)]
pub struct DeckOptions {
    /// Enable mouse capture (comfy-tabs click/scroll/reorder). Off by default so
    /// native terminal selection keeps working (L-T2).
    pub mouse_capture: bool,
    /// Structured debug log path (`OXAGEN_DEBUG=1`), or `None` for a no-op sink.
    pub debug_log_path: Option<std::path::PathBuf>,
}

impl Default for DeckOptions {
    fn default() -> Self {
        Self {
            mouse_capture: false,
            debug_log_path: None,
        }
    }
}

/// Run the command deck to completion. `Inbound` envelopes stream in over
/// `inbound`; the user's [`WorkspaceInput`]s stream out over `submissions`.
pub async fn run_deck(
    _opts: DeckOptions,
    mut inbound: UnboundedReceiver<Inbound>,
    _submissions: UnboundedSender<WorkspaceInput>,
) -> io::Result<()> {
    // Drain so a caller wiring the channel gets clean shutdown even pre-build.
    while inbound.recv().await.is_some() {}
    Ok(())
}
