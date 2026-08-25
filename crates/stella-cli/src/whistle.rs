//! Agent whistle: a cross-process broadcast that steers every live
//! non-interactive session on this machine at once.
//!
//! `stella-core`'s `TurnSteering` port already lets one process inject text
//! into a running turn at its next safe boundary (`stella-tui`'s `>` and
//! `stella-serve`'s `POST /v1/turns/{id}/steer` both do it in-process or over
//! HTTP). What was missing for a plain `stella run`/`stella goal` — the
//! session type most likely to be "running a full workspace compile and
//! eating your machine" — was any way to reach that port from a SECOND
//! process. This module is that reach: a small Unix domain socket per
//! session, bound inside the session's existing sidecar directory
//! (`stella_store::SessionRegistry::whistle_socket_path`), bridging into a
//! [`tap::HeadlessSteerTap`] that becomes that session's real
//! `stella_core::ports::TurnSteering` implementation.
//!
//! Unix-only for now (`tokio::net::UnixListener` has no Windows
//! counterpart, and Stella ships no Windows binary yet — see AGENTS.md's
//! Windows section). `stella whistle` on a non-Unix build reports the gap
//! rather than silently doing nothing.
//!
//! Not reachable yet: interactive mode. Its
//! steering tap is minted fresh per turn inside `command_deck.rs`, a file
//! closed to growth under the file-size ratchet (AGENTS.md "God files"), so
//! wiring a listener into it needs either a small `file-size-update` or the
//! per-turn tap construction moved into a sibling module first — tracked as
//! a follow-up rather than done here under time pressure. `stella-serve`
//! turns are also out of scope: that surface already has its own reach
//! (`POST /v1/turns/{id}/steer`) over a different transport (HTTP).

pub(crate) mod cmd;
#[cfg(unix)]
pub(crate) mod listener;
pub(crate) mod tap;
pub(crate) mod wire;

/// Start `session_id`'s whistle listener, if this platform has one, and
/// keep it alive for as long as the returned guard is held. Best-effort and
/// silent on failure — the same posture as
/// `crate::agent::presence::SessionPresence` — so a session that cannot be
/// reached this way simply is not whistleable, never a reason to fail the
/// run it belongs to.
///
/// The one call this crate's non-interactive doors need: they hold neither
/// `stella_store::SessionRegistry` nor a platform check of their own, and
/// shouldn't have to just to publish one control socket.
#[cfg(unix)]
pub(crate) fn spawn_for_session(
    session_id: &str,
    tap: std::sync::Arc<dyn tap::Whistleable>,
) -> Option<listener::WhistleListener> {
    listener::WhistleListener::spawn(
        &stella_store::SessionRegistry::open_default(),
        session_id,
        tap,
    )
}

/// As the `#[cfg(unix)]` twin above, on a platform `tokio::net`'s Unix
/// sockets don't reach — see this module's doc comment.
#[cfg(not(unix))]
pub(crate) fn spawn_for_session(
    _session_id: &str,
    _tap: std::sync::Arc<dyn tap::Whistleable>,
) -> Option<()> {
    None
}
