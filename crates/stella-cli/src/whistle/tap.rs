//! The `Whistleable` seam: what a whistle connection pushes into, whatever
//! `stella_core::ports::TurnSteering` implementation backs the session it
//! reached.
//!
//! A separately named trait rather than reusing `TurnSteering` itself,
//! because that port only exposes the *drain* half (what the engine reads
//! at a step boundary) — it deliberately offers no push method, so nothing
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
}

/// A minimal steering tap for a headless (non-deck) session: `stella run`
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
        // Whistle never sets this (see the module docs on scope) — it
        // stays wired only because the port requires it, and reading
        // `false` forever is the honest answer for a tap nothing latches.
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

    #[test]
    fn soft_stop_is_never_requested() {
        let tap = HeadlessSteerTap::default();
        tap.push("anything".to_string());
        assert!(!tap.soft_stop_requested());
    }
}
