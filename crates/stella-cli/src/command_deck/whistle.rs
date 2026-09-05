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
//! A whistle can also carry a stop (`>>> @agents ! …`). That one does
//! not go into a tap here: it leaves as `WorkspaceInput::Interrupt` down the
//! driver's own input channel, so a whistled stop and a typed `!` take one
//! path through the backlog and the pause gate rather than two.
//!
//! # Why this is a sibling module
//!
//! `command_deck.rs` is a grandfathered god file closed to growth (AGENTS.md
//! § "God files — plan around them, never into them"), and the deck's own
//! `steering.rs` is the template: the driver keeps the call, the reasoning
//! lives here. The boot spends one line, the in-deck session switch one, and
//! the per-turn construction is the line that was already there.

use std::sync::{Arc, Mutex, Weak};

use tokio::sync::mpsc::UnboundedSender;

use crate::subsession::SteeringTap;
use crate::whistle::tap::Whistleable;
use stella_tui::WorkspaceInput;

/// The publication point a deck session's whistle socket writes into.
///
/// Held by the driver loop for the session's lifetime. Steers reach the
/// running turn's tap when there is one and it can still act on them, and are
/// held for the next turn otherwise.
pub(super) struct DeckWhistle {
    /// The turn currently able to drain a steer.
    ///
    /// `Weak`, not `Arc`: the driver's per-turn `steering` local is what owns
    /// the tap, and holding a strong reference here would keep a finished
    /// turn's tap alive past the loop iteration that made it.
    live: Mutex<Weak<SteeringTap>>,
    /// Steers that arrived with no turn to take them, oldest first.
    pending: Mutex<Vec<String>>,
    /// The worker lanes this session drives, announced as each spawns
    /// ([`crate::subsession::LaneTapSink`]) — what a deep whistle reaches
    /// beyond the lead. `Weak` for the reason `live` is: the driver's
    /// `SubSessions` owns each tap and drops it with the lane, and a lane
    /// that ended must read as gone here rather than as a queue nobody
    /// drains. A deep whistle at an idle lane set is not buffered: a lane
    /// that does not exist yet has no work the guidance could be about.
    lanes: Mutex<Vec<Weak<SteeringTap>>>,
    /// This session's bound socket. Replaced when the deck switches sessions;
    /// dropping it unbinds and removes the socket file.
    listener: Mutex<Option<crate::whistle::SessionListener>>,
    /// The driver's own input channel — the one the deck sends keystrokes
    /// down. A delivered interrupt leaves by it as
    /// [`WorkspaceInput::Interrupt`], so the stop, the front insert and the
    /// resume are the path a typed `!` already takes
    /// (`command_deck::steer::interrupt_lead`) rather than a second one.
    input: UnboundedSender<WorkspaceInput>,
}

impl DeckWhistle {
    /// A relay bound to no socket, writing interrupts into `input`.
    fn new(input: UnboundedSender<WorkspaceInput>) -> Self {
        Self {
            live: Mutex::default(),
            pending: Mutex::default(),
            lanes: Mutex::default(),
            listener: Mutex::default(),
            input,
        }
    }

    /// Bind `session_id`'s whistle socket and keep it for as long as the
    /// returned relay is held.
    pub(super) fn spawn(session_id: &str, input: UnboundedSender<WorkspaceInput>) -> Arc<Self> {
        let relay = Arc::new(Self::new(input));
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

    /// [`Self::deliver`] to the lead, then [`Self::deliver_lanes`] to the
    /// workers it drives.
    fn deliver_deep(&self, text: String) {
        self.deliver(text.clone());
        self.deliver_lanes(text);
    }

    /// The same text into every worker lane still able to drain one. Lanes
    /// that ended are dropped from the list on the way past, so it never
    /// grows with a session's history.
    fn deliver_lanes(&self, text: String) {
        let mut lanes = self.lanes.lock().unwrap_or_else(|p| p.into_inner());
        lanes.retain(|lane| lane.upgrade().is_some());
        for tap in lanes.iter().filter_map(Weak::upgrade) {
            if !tap.is_settling() {
                tap.push(text.clone());
            }
        }
    }

    /// Stop the lead's turn and run `text` next — the room form of the
    /// composer's bang (`>>> @agents ! …`).
    ///
    /// The words leave as [`WorkspaceInput::Interrupt`] rather than going
    /// into a tap here. The driver owns the backlog and the pause gate, and
    /// its arms already know how to stop a running turn, run the words at
    /// rest, and hand a lane a steer instead. `keep` is `None`: the record
    /// was written once by the session that sent the line, so a target that
    /// wrote another would give one sentence a copy per machine.
    ///
    /// A deep interrupt also reaches the live lanes, as words rather than as
    /// a stop. A lane's next prompt is the lead's, so a stop there would have
    /// nothing to run next. The envelope's `Interrupt` doc reads a worker
    /// lane the same way.
    fn interrupt_lead(&self, text: String, deep: bool) {
        if deep {
            self.deliver_lanes(text.clone());
        }
        let _ = self.input.send(WorkspaceInput::Interrupt {
            agent: super::LEAD.to_string(),
            texts: vec![text],
            keep: None,
        });
    }
}

impl crate::subsession::LaneTapSink for DeckWhistle {
    fn register(&self, tap: &Arc<SteeringTap>) {
        self.lanes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Arc::downgrade(tap));
    }
}

/// The deck's half of the composer's broadcast address (`>@all …`,
/// `>>> @agents !!! …`): send the words to the live sessions on this machine
/// over their whistle sockets, off the driver's event pump, and report every
/// outcome as one chrome note.
///
/// A steer never reaches this session over its own socket — words meant for
/// the room land here through the composer's ordinary route. **A stop does.**
/// A person who tells every agent on the machine to stop means their own one
/// too, so an interrupt addressed to the room is delivered here in process,
/// through the relay's own [`DeckWhistle::interrupt_lead`], while the socket
/// fan-out still skips this session. An interrupt addressed to one other id
/// leaves this session alone.
pub(super) fn broadcast_from_deck(
    broadcast: stella_tui::Broadcast,
    relay: &Arc<DeckWhistle>,
    own_session: &str,
    in_tx: &tokio::sync::mpsc::UnboundedSender<stella_tui::Inbound>,
) {
    let own = own_session.to_string();
    let in_tx = in_tx.clone();
    let here = broadcast.interrupt
        && broadcast
            .session
            .as_deref()
            .is_none_or(|id| id == own_session);
    if here {
        relay.interrupt_lead(broadcast.message.clone(), broadcast.deep);
    }
    tokio::spawn(async move {
        let registry = stella_store::SessionRegistry::open_default();
        let ids: Vec<String> = broadcast.session.into_iter().collect();
        let outcomes = crate::whistle::cmd::broadcast(
            &registry,
            &broadcast.message,
            &ids,
            broadcast.deep,
            broadcast.interrupt,
            Some(&own),
        )
        .await;
        let mut note = crate::whistle::cmd::summary(&outcomes);
        if here {
            note.push_str("\nstopping this session too — the room includes you");
        }
        let _ = in_tx.send(super::chrome_note(note));
    });
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

    fn push_deep(&self, text: String) {
        if let Some(relay) = self.0.upgrade() {
            relay.deliver_deep(text);
        }
    }

    fn interrupt(&self, text: String, deep: bool) {
        if let Some(relay) = self.0.upgrade() {
            relay.interrupt_lead(text, deep);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_core::ports::TurnSteering;
    use tokio::sync::mpsc::UnboundedReceiver;

    /// A relay bound to no socket, with the driver channel an interrupt
    /// leaves by.
    fn relay() -> (Arc<DeckWhistle>, UnboundedReceiver<WorkspaceInput>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Arc::new(DeckWhistle::new(tx)), rx)
    }

    /// **The witness (#4768).** A whistle delivered while the deck sits idle
    /// is in the next turn's tap the moment that turn opens.
    ///
    /// This is the case the deck's per-turn tap made unreachable and the
    /// reason a session-scoped relay had to exist: on `main` there is nothing
    /// for a listener to bind to between turns, so the message had nowhere to
    /// go at all.
    #[test]
    fn a_steer_that_arrives_between_turns_opens_the_next_one() {
        let (relay, _driver) = relay();
        RelayHandle(Arc::downgrade(&relay)).push("read the failing test".to_string());

        let tap = relay.mint_turn_tap();
        assert_eq!(tap.drain_steering(), vec!["read the failing test"]);
    }

    /// A whistle during a live turn goes to that turn, not to the backlog —
    /// the whole point of the socket being reachable mid-turn.
    #[test]
    fn a_steer_during_a_turn_reaches_the_turn_that_is_running() {
        let (relay, _driver) = relay();
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
        let (relay, _driver) = relay();
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
                let (tx, _driver) = tokio::sync::mpsc::unbounded_channel();
                let relay = DeckWhistle::spawn("dw1", tx);
                let mut stream = tokio::net::UnixStream::connect(&socket)
                    .await
                    .expect("the deck session must have bound a whistle socket");
                crate::whistle::wire::write_frame(
                    &mut stream,
                    &crate::whistle::wire::WhistleRequest {
                        text: "the deck is listening".to_string(),
                        deep: false,
                        interrupt: false,
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

    /// **The witness for `--deep`.** A deep whistle reaches the lead's turn
    /// and every live worker lane the session announced; a plain one reaches
    /// the lead alone; a lane that ended is gone from the fan-out, and one
    /// that is settling — past its last model step — takes nothing.
    #[test]
    fn a_deep_whistle_reaches_the_lead_and_every_live_lane() {
        use crate::subsession::LaneTapSink as _;
        let (relay, _driver) = relay();
        let lead = relay.mint_turn_tap();
        let lane_a: Arc<SteeringTap> = Arc::default();
        let lane_b: Arc<SteeringTap> = Arc::default();
        let settling: Arc<SteeringTap> = Arc::default();
        settling.mark_settling();
        for lane in [&lane_a, &lane_b, &settling] {
            relay.register(lane);
        }
        let ended: Arc<SteeringTap> = Arc::default();
        relay.register(&ended);
        drop(ended);

        RelayHandle(Arc::downgrade(&relay)).push("lead only".to_string());
        assert_eq!(lead.drain_steering(), vec!["lead only"]);
        assert!(
            lane_a.drain_steering().is_empty(),
            "a plain whistle stops at the lead"
        );

        RelayHandle(Arc::downgrade(&relay)).push_deep("everyone: stop touching main".to_string());
        assert_eq!(lead.drain_steering(), vec!["everyone: stop touching main"]);
        assert_eq!(
            lane_a.drain_steering(),
            vec!["everyone: stop touching main"]
        );
        assert_eq!(
            lane_b.drain_steering(),
            vec!["everyone: stop touching main"]
        );
        assert!(
            settling.drain_steering().is_empty(),
            "a settling lane has no boundary left to drain at"
        );
        assert_eq!(
            relay.lanes.lock().unwrap().len(),
            3,
            "the ended lane is pruned on the way past"
        );
    }

    /// The turn that ended owns nothing: its tap is dropped with the loop
    /// iteration that made it, and the relay must notice rather than push into
    /// a queue nothing will read again.
    #[test]
    fn a_finished_turns_tap_stops_receiving() {
        let (relay, _driver) = relay();
        drop(relay.mint_turn_tap());
        RelayHandle(Arc::downgrade(&relay)).push("the next turn should know".to_string());

        assert_eq!(
            relay.mint_turn_tap().drain_steering(),
            vec!["the next turn should know"]
        );
    }

    /// One session with a bound socket, and the relay behind it.
    #[cfg(unix)]
    fn live_session(
        registry: &stella_store::SessionRegistry,
        id: &str,
    ) -> (Arc<DeckWhistle>, UnboundedReceiver<WorkspaceInput>) {
        listed(registry, id);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (DeckWhistle::spawn(id, tx), rx)
    }

    /// Put one live session in the registry. It binds no socket of its own,
    /// so on its own it is a target `stella whistle` cannot reach.
    #[cfg(unix)]
    fn listed(registry: &stella_store::SessionRegistry, id: &str) {
        let mut record = stella_store::SessionRecord::new("ws".to_string(), "name".to_string());
        record.id = id.to_string();
        record.status = stella_store::SessionStatus::InProgress;
        registry.upsert(&record).expect("register the session");
    }

    /// The words of the one interrupt a driver channel was handed.
    #[cfg(unix)]
    fn taken(rx: &mut UnboundedReceiver<WorkspaceInput>) -> Vec<String> {
        match rx.try_recv() {
            Ok(WorkspaceInput::Interrupt { texts, keep, .. }) => {
                assert!(keep.is_none(), "the record is written once, by the sender");
                texts
            }
            other => panic!("expected one interrupt, got {other:?}"),
        }
    }

    /// The chrome notes the driver was told to show.
    #[cfg(unix)]
    fn notes(rx: &mut tokio::sync::mpsc::UnboundedReceiver<stella_tui::Inbound>) -> String {
        let mut out = Vec::new();
        while let Ok(inbound) = rx.try_recv() {
            if let stella_tui::Inbound::Event {
                event: stella_protocol::AgentEvent::Text { text },
                ..
            } = inbound
            {
                out.push(text);
            }
        }
        out.join("\n")
    }

    /// A short home under `/tmp`: a `sockaddr_un` path is capped at about a
    /// hundred bytes, and the platform temp dir is already half of that on
    /// macOS. The session ids are short for the same reason.
    #[cfg(unix)]
    fn short_home() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("bc")
            .tempdir_in("/tmp")
            .expect("home")
    }

    /// **The witness for the room broadcast.** `>>> @agents !!! …`
    /// stops every live session on this machine, and the one that sent it.
    /// Two fixture sessions take the stop over their real sockets, carrying
    /// the words; the sender takes the same stop in process rather than
    /// through its own socket; a session that bound none is reported and
    /// costs the others nothing.
    ///
    /// A broadcast that carries only words gives none of the three an
    /// interrupt to receive, which is what makes this a witness.
    #[cfg(unix)]
    #[test]
    fn a_room_broadcast_stops_every_live_session_and_the_sender() {
        let _env = crate::test_env::lock();
        let home = short_home();
        let _home = crate::test_env::home_sandbox(home.path());

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async {
                let registry = stella_store::SessionRegistry::open_default();
                let (_one, mut one_rx) = live_session(&registry, "bc1");
                let (_two, mut two_rx) = live_session(&registry, "bc2");
                listed(&registry, "bc3");
                let (sender, mut sender_rx) = live_session(&registry, "bc0");
                let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel();

                broadcast_from_deck(
                    stella_tui::Broadcast {
                        message: "do not force-push".to_string(),
                        session: None,
                        deep: true,
                        interrupt: true,
                        keep: Some(stella_tui::KeepStrength::Rule),
                    },
                    &sender,
                    "bc0",
                    &in_tx,
                );

                // The sender's own stop is in hand before any socket is
                // touched. The rest cross the accept loops' own tasks.
                assert_eq!(
                    taken(&mut sender_rx),
                    vec!["do not force-push".to_string()],
                    "the room includes the session that spoke"
                );
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;

                for rx in [&mut one_rx, &mut two_rx] {
                    let texts = taken(rx);
                    assert_eq!(texts.len(), 1);
                    assert!(
                        texts[0].ends_with("do not force-push"),
                        "the words ride with the stop: {texts:?}"
                    );
                }

                let said = notes(&mut in_rx);
                assert!(said.contains("delivered    bc1"), "{said}");
                assert!(said.contains("delivered    bc2"), "{said}");
                assert!(said.contains("unreachable  bc3"), "{said}");
                assert!(
                    !said.contains("bc0"),
                    "the sender is no socket target: {said}"
                );
                assert!(said.contains("stopping this session too"), "{said}");
            });
    }

    /// **The witness for the fan-out.** A session whose socket takes
    /// the frame and never answers holds up its own row alone. The live
    /// session has its stop long before the stalled one's ack timeout is
    /// anywhere near spent.
    #[cfg(unix)]
    #[test]
    fn a_stalled_session_does_not_hold_up_the_others_stop() {
        let _env = crate::test_env::lock();
        let home = short_home();
        let _home = crate::test_env::home_sandbox(home.path());

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async {
                let registry = stella_store::SessionRegistry::open_default();
                let (_live, mut live_rx) = live_session(&registry, "st1");
                // A socket nobody accepts on: connecting works, the frame is
                // written, and no ack ever comes.
                listed(&registry, "st2");
                registry.prepare_sidecar("st2").expect("sidecar");
                let _stalled = tokio::net::UnixListener::bind(registry.whistle_socket_path("st2"))
                    .expect("bind the stalled socket");
                let (sender, _sender_rx) = live_session(&registry, "st0");
                let (in_tx, _in_rx) = tokio::sync::mpsc::unbounded_channel();

                broadcast_from_deck(
                    stella_tui::Broadcast {
                        message: "stop touching main".to_string(),
                        session: None,
                        deep: false,
                        interrupt: true,
                        keep: None,
                    },
                    &sender,
                    "st0",
                    &in_tx,
                );

                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let texts = taken(&mut live_rx);
                assert!(
                    texts[0].ends_with("stop touching main"),
                    "the live session is not made to wait: {texts:?}"
                );
            });
    }
}
