//! The `Whistleable` seam: what a whistle connection pushes into, whatever
//! `stella_core::ports::TurnSteering` implementation backs the session it
//! reached.
//!
//! A separately named trait rather than reusing `TurnSteering` itself,
//! because that port only exposes the *drain* half (what the engine reads
//! at a step boundary) — it offers no push method, so nothing
//! outside a turn's own host can inject through it by accident. This trait
//! is that host-side push, and it is `stella-cli`-internal: `stella-core`
//! never learns whistle exists.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) trait Whistleable: Send + Sync {
    /// Queue `text` for injection at the session's next safe step boundary
    /// (`stella_core::driver::step_boundary::consult_hosts` — strictly
    /// between model calls, never mid-tool).
    fn push(&self, text: String);

    /// As [`Self::push`], and into every live worker lane this session
    /// drives as well. A tap that drives no lanes — every non-interactive
    /// door — has nothing deeper to reach, so the default is the lead alone.
    fn push_deep(&self, text: String) {
        self.push(text);
    }

    /// Queue `text` and stop the running turn, so the words run next rather
    /// than only riding along — the room form of the composer's bang
    /// (`>>> @agents ! …`).
    ///
    /// The default delivers the words and stops nothing, which is all a tap
    /// with no stop to latch can do: the text still lands, and no caller is
    /// told a turn halted when none did.
    fn interrupt(&self, text: String, deep: bool) {
        if deep {
            self.push_deep(text);
        } else {
            self.push(text);
        }
    }
}

/// A minimal steering tap for a non-interactive (non-deck) session: `stella run`
/// and `stella goal`, foreground or daemon-supervised alike — see
/// `crate::agent::run_turn`'s `controls` parameter.
///
/// Mirrors `stella-serve`'s `SteerQueue` (`crates/stella-serve/src/controls.rs`)
/// and the deck's `crate::subsession::SteeringTap`, which both implement
/// this identical `Mutex<Vec<String>>` + latched `AtomicBool` shape
/// independently already. A third small copy here is cheaper than making
/// either of those reach across a crate boundary it otherwise has no reason
/// to cross; `Whistleable` is what lets all three be driven the same way
/// without merging them.
#[derive(Default)]
pub(crate) struct HeadlessSteerTap {
    queue: Mutex<Vec<String>>,
    soft_stop: AtomicBool,
}

impl Whistleable for HeadlessSteerTap {
    fn push(&self, text: String) {
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(text);
    }

    /// The words first, then the stop. The boundary drains steering before it
    /// reads the stop (`stella_core::driver::step_boundary::consult_hosts`),
    /// so this order is what puts the text in the turn it halts.
    ///
    /// The latch holds until the run ends, as `SteeringTap`'s does. A door
    /// with no deck is one arc: `stella run` is a turn, and a `stella goal`
    /// round that aborts ends the arc (`stella_core::goal`). So "stop" here
    /// means the session stops, which is what a person broadcasting a bang
    /// asked every session to do.
    fn interrupt(&self, text: String, _deep: bool) {
        self.push(text);
        self.soft_stop.store(true, Ordering::SeqCst);
    }
}

impl stella_core::ports::TurnSteering for HeadlessSteerTap {
    fn drain_steering(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .queue
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    fn soft_stop_requested(&self) -> bool {
        // Latched by an interrupt whistle alone (`Whistleable::interrupt`).
        // A plain steer rides along and never stops the turn.
        self.soft_stop.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_core::ports::TurnSteering;

    #[test]
    fn pushed_text_drains_oldest_first_and_empties() {
        let tap = HeadlessSteerTap::default();
        tap.push("first".to_string());
        tap.push("second".to_string());
        assert_eq!(tap.drain_steering(), vec!["first", "second"]);
        assert!(tap.drain_steering().is_empty());
    }

    /// **The witness for the stop at a door with no deck.** A plain steer
    /// rides along; an interrupt puts the words in the turn and then halts
    /// it, in that order.
    #[test]
    fn only_an_interrupt_stops_a_headless_turn() {
        let tap = HeadlessSteerTap::default();
        tap.push("ride along".to_string());
        assert!(!tap.soft_stop_requested(), "a steer stops nothing");

        tap.interrupt("stop touching main".to_string(), true);
        assert_eq!(
            tap.drain_steering(),
            vec!["ride along", "stop touching main"],
            "the words are in the turn the stop halts"
        );
        assert!(tap.soft_stop_requested());
    }
}
