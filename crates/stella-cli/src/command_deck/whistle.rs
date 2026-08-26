// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The deck's session-scoped whistle relay (#4768).
//!
//! Every other door `stella whistle` reaches hands the listener the very tap
//! its engine drains, because those doors have one: a `stella run` or a
//! `stella goal` arc builds a steering tap once and keeps it for the whole
//! run. The deck does not. Its tap is minted per turn — a soft stop latched
//! in one turn must not leak into the next — and between turns there is no
//! tap at all, which is most of a deck session's life: the lead sits at
//! `NeedsInput` waiting for a person to type.
//!
//! A socket bound to whichever tap happened to exist at boot would therefore
//! steer one turn and then quietly steer nothing. So the socket is bound to
//! [`DeckWhistle`], which outlives every turn and forwards to whichever tap is
//! currently able to drain — buffering when none is, so a whistle at an idle
//! deck is delivered rather than refused, and lands in front of the model the
//! moment the next turn opens.
//!
//! # Why this is a sibling module
//!
//! `command_deck.rs` is a grandfathered god file closed to growth (AGENTS.md
//! § "God files — plan around them, never into them"), and the deck's own
//! `steering.rs` is the template: the driver keeps the call, the reasoning
//! lives here. The boot spends one line, the in-deck session switch one, and
//! the per-turn construction is the line that was already there.

use std::sync::{Arc, Mutex, Weak};

use crate::subsession::SteeringTap;
use crate::whistle::tap::Whistleable;

/// The publication point a deck session's whistle socket writes into.
///
/// Held by the driver loop for the session's lifetime. Steers reach the
/// running turn's tap when there is one and it can still act on them, and are
/// held for the next turn otherwise.
#[derive(Default)]
pub(super) struct DeckWhistle {
    /// The turn currently able to drain a steer.
    ///
    /// `Weak`, not `Arc`: the driver's per-turn `steering` local is what owns
    /// the tap, and holding a strong reference here would keep a finished
    /// turn's tap alive past the loop iteration that made it.
    live: Mutex<Weak<SteeringTap>>,
    /// Steers that arrived with no turn to take them, oldest first.
    pending: Mutex<Vec<String>>,
    /// This session's bound socket. Replaced when the deck switches sessions;
    /// dropping it unbinds and removes the socket file.
    listener: Mutex<Option<crate::whistle::SessionListener>>,
}

impl DeckWhistle {
    /// Bind `session_id`'s whistle socket and keep it for as long as the
    /// returned relay is held.
    pub(super) fn spawn(session_id: &str) -> Arc<Self> {
        let relay = Arc::new(Self::default());
        relay.rebind(session_id);
        relay
    }

    /// Move the socket to `session_id` — the in-deck session switch, where the
    /// driver re-keys the journal, the durability binding and the lead's claim
    /// holder alike. Without this, a switched deck would keep answering
    /// whistles aimed at the session it left.
    pub(super) fn rebind(self: &Arc<Self>, session_id: &str) {
        let handle: Arc<dyn Whistleable> = Arc::new(RelayHandle(Arc::downgrade(self)));
        let next = crate::whistle::spawn_for_session(session_id, handle);
        // Assigned rather than cleared first, so the new socket is bound
        // before the old one is unbound: the ids differ, so the two paths
        // never collide.
        *self.listener.lock().unwrap_or_else(|p| p.into_inner()) = next;
    }

    /// The tap for a turn about to start, carrying whatever arrived while the
    /// deck was idle.
    ///
    /// Per-turn by construction, which is what keeps a soft stop latched here
    /// from leaking into the next turn. `Arc` because the turn runner also
    /// publishes a clone to the registry, so sub-agents this turn dispatches
    /// stop when it does (`crate::subagent`); the engine still takes it by
    /// reference.
    pub(super) fn mint_turn_tap(&self) -> Arc<SteeringTap> {
        let tap: Arc<SteeringTap> = Arc::default();
        let mut live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        for text in pending.drain(..) {
            tap.push(text);
        }
        *live = Arc::downgrade(&tap);
        tap
    }

    /// Route one delivered steer: to the running turn when it can still drain
    /// one, to the backlog otherwise.
    ///
    /// A *settling* turn counts as no turn. Its model steps are over and it is
    /// only finishing bookkeeping ([`SteeringTap::mark_settling`]), so text
    /// pushed into its queue would be drained by nobody — the same fact
    /// `command_deck::steer::steer_lead` reads before deciding whether a typed
    /// `>` steers the turn or joins the queue.
    fn deliver(&self, text: String) {
        let live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        match live.upgrade() {
            Some(tap) if !tap.is_settling() => tap.push(text),
            _ => self
                .pending
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(text),
        }
    }
}

/// What the listener actually holds: a weak handle back to the relay.
///
/// The listener lives *inside* [`DeckWhistle`], so handing the accept loop an
/// `Arc<DeckWhistle>` would close a cycle — the relay would never be dropped,
/// and the `Drop` that unbinds the socket would never run. A `Weak` breaks it,
/// and reads correctly besides: a socket whose session is gone has nothing to
/// deliver to.
struct RelayHandle(Weak<DeckWhistle>);

impl Whistleable for RelayHandle {
    fn push(&self, text: String) {
        if let Some(relay) = self.0.upgrade() {
            relay.deliver(text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_core::ports::TurnSteering;

    /// **The witness (#4768).** A whistle delivered while the deck sits idle
    /// is in the next turn's tap the moment that turn opens.
    ///
    /// This is the case the deck's per-turn tap made unreachable and the
    /// reason a session-scoped relay had to exist: on `main` there is nothing
    /// for a listener to bind to between turns, so the message had nowhere to
    /// go at all.
    #[test]
    fn a_steer_that_arrives_between_turns_opens_the_next_one() {
        let relay = Arc::new(DeckWhistle::default());
        RelayHandle(Arc::downgrade(&relay)).push("read the failing test".to_string());

        let tap = relay.mint_turn_tap();
        assert_eq!(tap.drain_steering(), vec!["read the failing test"]);
    }

    /// A whistle during a live turn goes to that turn, not to the backlog —
    /// the whole point of the socket being reachable mid-turn.
    #[test]
    fn a_steer_during_a_turn_reaches_the_turn_that_is_running() {
        let relay = Arc::new(DeckWhistle::default());
        let tap = relay.mint_turn_tap();
        RelayHandle(Arc::downgrade(&relay)).push("stop the compile".to_string());

        assert_eq!(tap.drain_steering(), vec!["stop the compile"]);
        assert!(
            relay.mint_turn_tap().drain_steering().is_empty(),
            "a steer the running turn took must not be replayed into the next"
        );
    }

    /// A settling turn is past its last model step, so a steer pushed into it
    /// would be drained by nobody. It waits for the next turn instead — the
    /// rule `steer_lead` already applies to a typed `>`.
    #[test]
    fn a_steer_at_a_settling_turn_waits_for_the_next_one() {
        let relay = Arc::new(DeckWhistle::default());
        let settling = relay.mint_turn_tap();
        settling.mark_settling();
        RelayHandle(Arc::downgrade(&relay)).push("next time, run the tests".to_string());

        assert!(
            settling.drain_steering().is_empty(),
            "a turn that is only finishing bookkeeping cannot act on a steer"
        );
        assert_eq!(
            relay.mint_turn_tap().drain_steering(),
            vec!["next time, run the tests"]
        );
    }

    /// **The cross-process witness (#4768).** A message sent over a deck
    /// session's real socket — by the frames `stella whistle` sends, from
    /// outside the turn loop — reaches the next turn's tap.
    ///
    /// The relay tests above prove the routing; this proves there is a socket
    /// to route from, which is the half `main` does not have: nothing binds
    /// one for a deck session at all.
    #[cfg(unix)]
    #[test]
    fn a_message_sent_over_a_deck_sessions_socket_reaches_its_next_turn() {
        let _env = crate::test_env::lock();
        // Under `/tmp`: a `sockaddr_un` path is capped at ~104 bytes and the
        // platform temp dir is already half of that on macOS. The session id
        // is short for the same reason — it is a path component.
        let home = tempfile::Builder::new()
            .prefix("dw")
            .tempdir_in("/tmp")
            .expect("home");
        let _home = crate::test_env::home_sandbox(home.path());
        let socket = stella_store::SessionRegistry::open_default().whistle_socket_path("dw1");

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async {
                let relay = DeckWhistle::spawn("dw1");
                let mut stream = tokio::net::UnixStream::connect(&socket)
                    .await
                    .expect("the deck session must have bound a whistle socket");
                crate::whistle::wire::write_frame(
                    &mut stream,
                    &crate::whistle::wire::WhistleRequest {
                        text: "the deck is listening".to_string(),
                    },
                )
                .await
                .expect("write the frame");
                let ack: crate::whistle::wire::WhistleAck =
                    crate::whistle::wire::read_frame(&mut stream)
                        .await
                        .expect("ack");
                assert!(ack.delivered);

                // The push crosses the accept loop's own task, not the
                // connection future awaited above.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                assert_eq!(
                    relay.mint_turn_tap().drain_steering(),
                    vec!["the deck is listening"]
                );
            });
    }

    /// The turn that ended owns nothing: its tap is dropped with the loop
    /// iteration that made it, and the relay must notice rather than push into
    /// a queue nothing will read again.
    #[test]
    fn a_finished_turns_tap_stops_receiving() {
        let relay = Arc::new(DeckWhistle::default());
        drop(relay.mint_turn_tap());
        RelayHandle(Arc::downgrade(&relay)).push("still worth saying".to_string());

        assert_eq!(
            relay.mint_turn_tap().drain_steering(),
            vec!["still worth saying"]
        );
    }
}
