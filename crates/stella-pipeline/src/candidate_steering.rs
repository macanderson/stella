//! Fan one steering tap out to every best-of-N candidate.
//!
//! [`stella_core::ports::TurnSteering::drain_steering`] is destructive by
//! contract — "whatever this returns WILL be injected, so the implementation
//! owns dedup". Handing the caller's tap straight to N candidate engines
//! therefore makes them race for the user's words: the first candidate to
//! reach a step boundary takes the steer and every later candidate runs as if
//! the user had never typed. Candidates run sequentially, so in practice
//! candidate 1 answered the steer and candidates 2..N did not — and when the
//! judge picked one of those, the steer was silently absent from the winning
//! transcript.
//!
//! A steer is an instruction about the *turn*, not about whichever candidate
//! happened to be executing when it was typed. So this mirrors the upstream tap
//! into an append-only log and gives each candidate its own read cursor over
//! it: every candidate observes every steer, in order, exactly once —
//! including steers that arrived before that candidate started.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use stella_core::ports::TurnSteering;

/// The shared mirror. One per fan-out; hand each candidate a
/// [`SteeringFanOut::candidate`] view.
pub(crate) struct SteeringFanOut<'a> {
    upstream: &'a dyn TurnSteering,
    /// Every steer this fan-out has ever pulled upstream, oldest first. Append
    /// -only: a cursor into it stays valid, which is what lets a candidate that
    /// starts late still replay what it missed.
    seen: Mutex<Vec<String>>,
}

impl<'a> SteeringFanOut<'a> {
    pub(crate) fn new(upstream: &'a dyn TurnSteering) -> Self {
        Self {
            upstream,
            seen: Mutex::new(Vec::new()),
        }
    }

    /// A fresh view, positioned at the START of the log rather than its end —
    /// a candidate spawned after the user typed must still see what they said.
    pub(crate) fn candidate(&self) -> CandidateSteering<'_> {
        CandidateSteering {
            fan: self,
            cursor: AtomicUsize::new(0),
        }
    }

    /// How many distinct steers have been mirrored — for the caller's
    /// reporting, never for control flow.
    #[cfg(test)]
    fn mirrored(&self) -> usize {
        self.seen.lock().unwrap_or_else(|p| p.into_inner()).len()
    }
}

/// One candidate's view of the fan-out: the shared log plus a private cursor.
pub(crate) struct CandidateSteering<'a> {
    fan: &'a SteeringFanOut<'a>,
    cursor: AtomicUsize,
}

impl TurnSteering for CandidateSteering<'_> {
    fn drain_steering(&self) -> Vec<String> {
        // Pull upstream under the log lock so two candidates cannot both
        // consume from the destructive tap and split one steer between them.
        let mut log = self
            .fan
            .seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        log.extend(self.fan.upstream.drain_steering());
        let from = self.cursor.swap(log.len(), Ordering::SeqCst);
        // `from` can only exceed the length if the log shrank, which it never
        // does — clamped anyway so a future change cannot panic a live turn.
        log[from.min(log.len())..].to_vec()
    }

    fn soft_stop_requested(&self) -> bool {
        // A latch, and a turn-wide one: Esc ends every candidate, so this
        // passes straight through rather than being fanned.
        self.fan.upstream.soft_stop_requested()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tap that hands out its queue once, like the deck's real one.
    struct OnceTap {
        queue: Mutex<Vec<String>>,
        stop: bool,
    }

    impl OnceTap {
        fn with(items: &[&str]) -> Self {
            Self {
                queue: Mutex::new(items.iter().map(|s| s.to_string()).collect()),
                stop: false,
            }
        }
    }

    impl TurnSteering for OnceTap {
        fn drain_steering(&self) -> Vec<String> {
            std::mem::take(&mut *self.queue.lock().unwrap())
        }
        fn soft_stop_requested(&self) -> bool {
            self.stop
        }
    }

    /// The bug this module exists to fix: with the bare tap the second reader
    /// gets nothing. Every candidate must see the steer.
    #[test]
    fn every_candidate_sees_the_same_steer() {
        let tap = OnceTap::with(&["also check the tests"]);
        let fan = SteeringFanOut::new(&tap);

        let first = fan.candidate();
        let second = fan.candidate();
        let third = fan.candidate();

        assert_eq!(first.drain_steering(), vec!["also check the tests"]);
        assert_eq!(
            second.drain_steering(),
            vec!["also check the tests"],
            "candidate 2 must not lose the steer to candidate 1"
        );
        assert_eq!(
            third.drain_steering(),
            vec!["also check the tests"],
            "and neither must candidate 3"
        );
        assert_eq!(
            fan.mirrored(),
            1,
            "the steer is mirrored once, not per read"
        );
    }

    /// A candidate created AFTER the steer was already pulled still replays it
    /// — candidates run sequentially, so this is the common case, not an edge.
    #[test]
    fn a_candidate_that_starts_late_replays_what_it_missed() {
        let tap = OnceTap::with(&["use the existing helper"]);
        let fan = SteeringFanOut::new(&tap);

        let early = fan.candidate();
        assert_eq!(early.drain_steering(), vec!["use the existing helper"]);

        // Candidate 2 is constructed only now, after the upstream tap is empty.
        let late = fan.candidate();
        assert_eq!(
            late.drain_steering(),
            vec!["use the existing helper"],
            "a candidate that started after the user typed must still see it"
        );
    }

    /// Within one candidate a steer is injected exactly once — the cursor has
    /// to advance, or every step would re-inject the whole history.
    #[test]
    fn one_candidate_never_sees_the_same_steer_twice() {
        let tap = OnceTap::with(&["rename it to `parse`"]);
        let fan = SteeringFanOut::new(&tap);
        let candidate = fan.candidate();

        assert_eq!(candidate.drain_steering(), vec!["rename it to `parse`"]);
        assert!(
            candidate.drain_steering().is_empty(),
            "a second boundary with nothing new must inject nothing"
        );
    }

    /// Steers keep arriving mid-run; ordering and per-candidate exactly-once
    /// both have to survive interleaving.
    #[test]
    fn later_steers_reach_every_candidate_in_order() {
        struct Staged {
            batches: Mutex<std::collections::VecDeque<Vec<String>>>,
        }
        impl TurnSteering for Staged {
            fn drain_steering(&self) -> Vec<String> {
                self.batches.lock().unwrap().pop_front().unwrap_or_default()
            }
            fn soft_stop_requested(&self) -> bool {
                false
            }
        }

        let tap = Staged {
            batches: Mutex::new(
                [vec!["first".to_string()], vec!["second".to_string()]]
                    .into_iter()
                    .collect(),
            ),
        };
        let fan = SteeringFanOut::new(&tap);

        let a = fan.candidate();
        assert_eq!(a.drain_steering(), vec!["first"]);
        assert_eq!(a.drain_steering(), vec!["second"]);

        // A candidate starting now sees BOTH, in the order the user typed them.
        let b = fan.candidate();
        assert_eq!(b.drain_steering(), vec!["first", "second"]);
    }

    /// The soft stop is turn-wide, so it must reach every candidate view
    /// unchanged rather than being consumed by the first one to ask.
    #[test]
    fn the_soft_stop_passes_through_to_every_candidate() {
        let tap = OnceTap {
            queue: Mutex::new(Vec::new()),
            stop: true,
        };
        let fan = SteeringFanOut::new(&tap);
        assert!(fan.candidate().soft_stop_requested());
        assert!(fan.candidate().soft_stop_requested());
    }
}
