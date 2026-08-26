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
//! Interactive mode reaches it too, through a relay rather than a tap
//! (`crate::command_deck::whistle`, #4768): the deck mints a fresh
//! `SteeringTap` per turn and has none at all between turns, so its socket is
//! bound to a session-scoped publication point that forwards to whichever tap
//! can currently drain one.
//!
//! `stella-serve` turns are out of scope: that surface is a separate binary
//! with its own reach (`POST /v1/turns/{id}/steer`) over a different
//! transport, and a served session registers in no registry `stella whistle`
//! can enumerate — #4770 is where reaching it is being decided.

pub(crate) mod cmd;
#[cfg(unix)]
pub(crate) mod listener;
pub(crate) mod tap;
pub(crate) mod wire;

/// What [`spawn_for_session`] hands back, named once so a caller storing the
/// guard in a struct field does not have to name whichever type its build
/// has. Dropping it unbinds and removes the socket.
#[cfg(unix)]
pub(crate) type SessionListener = listener::WhistleListener;

/// As the `#[cfg(unix)]` twin above — there is no socket to release, so there
/// is nothing to hold.
#[cfg(not(unix))]
pub(crate) type SessionListener = ();

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
