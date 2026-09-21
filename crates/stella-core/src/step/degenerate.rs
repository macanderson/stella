//! Catches a stream that keeps arriving but has stopped saying anything.
//!
//! The idle bound in [`crate::step::bounded_generation`] asks one question:
//! has anything arrived lately? A broken stream answers yes every time.
//! Fragments keep landing. The clock keeps resetting. The call runs until a
//! person kills it.
//!
//! One run showed the shape. The model was `moonshotai/kimi-k3` on
//! OpenRouter, served upstream by Fireworks. It wrote `The`, then one `!`
//! over and over past 16KB. The turn ran 272 seconds at output token rates.
//! A person pressed Esc to end it.
//!
//! Why `!`? These models read and write bytes through a BPE table. Token 0
//! in that table is `!`. Both `cl100k_base` and `o200k_base` agree. When the
//! math inside a serving host goes bad, its token scores turn to NaN. The
//! top score is then token 0. So the model writes `!` and never stops. A
//! gateway can route to a host like that and say nothing about it.
//!
//! The text reads fine right up to the cliff. So this module does not judge
//! meaning at all. It checks one rule: no good answer writes the same
//! character [`RUN_LIMIT`] times in a row.
//!
//! A repeated *phrase* is a different bug, with a different cause and real
//! false alarms. Tables, lists, and near-matching diff hunks all repeat.
//! [`crate::loop_detect`] already owns repeats across calls. This owns the
//! one in-stream shape that can only be a fault.
//!
//! The guard sits next to the idle bound, not inside an adapter. Every
//! dialect shares the observer port. One guard here covers Anthropic,
//! OpenAI, Gemini, Bedrock, and every OpenAI-compatible gateway. Five
//! per-adapter copies would drift apart.
//!
//! Not every answer arrives through that port, though. An adapter whose
//! stream recovery has latched sends a plain unary request instead, and
//! returns the whole answer without calling the observer once. Bedrock is
//! unary by construction and calls the observer itself, but the fallback
//! path in the streaming adapters does not. So the check runs a second time
//! where a dispatch finishes, over an answer the watch never saw. See
//! [`crate::step::StreamProgress::settle`], which is where both readings
//! live.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use stella_protocol::ProviderError;
use tokio::sync::Notify;

/// How many of one character in a row mark a stream as broken.
///
/// Set high enough that real text cannot reach it. The longest run real text
/// makes is a rule or a table edge. A terminal caps those near 100. The one
/// fault seen cleared 1024 inside its first fragment. So a limit this high
/// still trips in well under a second. Lowering it buys no accuracy. It only
/// buys false alarms.
pub(crate) const RUN_LIMIT: u32 = 1024;

/// The error a tripped stream ends on.
///
/// It is [`ProviderError::Terminal`] on purpose. `Transport` is retryable, so
/// that spelling would hand a broken host the same unbounded wait again, once
/// per attempt.
///
/// This lives here, not at a call site, because two paths end a stream this
/// way: the engine's own bound, and the plain calls in
/// [`crate::accounted_call`]. Two copies of one sentence drift apart.
pub(crate) fn terminal_error() -> ProviderError {
    ProviderError::Terminal(format!(
        "generation degenerated: the stream repeated one character {RUN_LIMIT} times and said \
         nothing else. This is a fault in the serving host, not a refusal by the model. On a \
         gateway, set `upstream_pin` in `[providers.<id>]` to keep this session off the \
         endpoint that produced it"
    ))
}

/// Counts one character repeating across a call's stream fragments. It
/// latches once the count reaches [`RUN_LIMIT`].
///
/// It takes no lock, by design. [`stella_protocol::ToolCallObserver`] runs
/// its methods on the task that polls the provider stream. That task must
/// never wait on a lock the finish path wants. Holding no lock is the
/// cheapest way to obey. The state fits in one `AtomicU64`. The character
/// sits in the top 32 bits. Its count sits in the low 32. Feeding a fragment
/// is one load, one scan, and one store.
///
/// One task feeds it: the stream poller. So the load and store need no CAS.
/// A torn read can lose the tail of a run and delay the trip by a fragment.
/// It cannot invent one.
#[derive(Default)]
pub(crate) struct DegenerateWatch {
    /// Packed `(char, run_len)`. `run_len == 0` means nothing seen yet.
    run: AtomicU64,
    /// Set once the limit is reached. The trip is reported once, and stays
    /// set for a reader that arrives later.
    tripped: AtomicBool,
    /// Wakes the waiter in [`DegenerateWatch::tripped`]. This uses
    /// `notify_one`, not `notify_waiters`. Only `notify_one` saves a permit
    /// when nobody waits yet. The trip can land before the bound starts to
    /// wait. A lost wake there would let the bad stream run on.
    trip: Notify,
}

impl fmt::Debug for DegenerateWatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DegenerateWatch")
            .field("tripped", &self.is_tripped())
            .finish_non_exhaustive()
    }
}

fn pack(ch: u32, run: u32) -> u64 {
    (u64::from(ch) << 32) | u64::from(run)
}

fn unpack(bits: u64) -> (u32, u32) {
    ((bits >> 32) as u32, bits as u32)
}

impl DegenerateWatch {
    /// Feed one stream fragment. Returns `true` the first time the count
    /// crosses [`RUN_LIMIT`]. Returns `false` every other time, later
    /// fragments of a tripped stream included. So one fault is reported
    /// once.
    pub(crate) fn feed(&self, delta: &str) -> bool {
        if self.is_tripped() {
            return false;
        }
        let (mut ch, mut run) = unpack(self.run.load(Ordering::Relaxed));
        let mut crossed = false;
        for c in delta.chars() {
            let c = u32::from(c);
            if run > 0 && c == ch {
                run = run.saturating_add(1);
            } else {
                ch = c;
                run = 1;
            }
            if run >= RUN_LIMIT {
                crossed = true;
                break;
            }
        }
        self.run.store(pack(ch, run), Ordering::Relaxed);
        if crossed && !self.tripped.swap(true, Ordering::Relaxed) {
            self.trip.notify_one();
            return true;
        }
        false
    }

    /// Clear the count and the mark, for the start of a new attempt.
    ///
    /// A run belongs to one stream. A retry opens a new one, often on a
    /// different host. The count from the last attempt must not be charged
    /// against it: a call that ends on 1000 dashes and a retry that opens on
    /// 30 more would add up to a trip neither stream earned.
    ///
    /// Clearing the mark is safe because the waiter is built inside the
    /// attempt and dies with it. No waiter outlives the stream it watches.
    ///
    /// The saved wake has to go with it. `notify_one` stores a permit when no
    /// waiter has arrived, and that permit outlives the attempt that earned
    /// it: a trip the last attempt's waiter never took would fire this
    /// attempt's waiter at once and cut a healthy retry. `enable` registers
    /// the future and takes the permit if one is stored, and the future is
    /// dropped here rather than awaited, so the permit is discarded instead
    /// of passed on.
    pub(crate) fn reset(&self) {
        self.run.store(0, Ordering::Relaxed);
        self.tripped.store(false, Ordering::Relaxed);
        let notified = self.trip.notified();
        std::pin::pin!(notified).as_mut().enable();
    }

    /// Whether this stream has been marked broken.
    pub(crate) fn is_tripped(&self) -> bool {
        self.tripped.load(Ordering::Relaxed)
    }

    /// Whether this stream has fed the watch any text at all.
    ///
    /// A run length of zero is the packed state [`DegenerateWatch::reset`]
    /// leaves behind, and [`DegenerateWatch::feed`] sets a length of at least
    /// one for any text it is given. So this answers for the current stream
    /// rather than for the call, which is what the caller needs: it asks
    /// whether there is an answer here the watch has never seen.
    pub(crate) fn saw_nothing(&self) -> bool {
        unpack(self.run.load(Ordering::Relaxed)).1 == 0
    }

    /// Waits for the stream to be marked broken. Returns at once if it
    /// already is. That first check is not a shortcut. It closes the gap
    /// between a trip and a waiter arriving. The saved `notify_one` permit
    /// closes the same gap from the other side.
    pub(crate) async fn tripped(&self) {
        if self.is_tripped() {
            return;
        }
        self.trip.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_run_of_one_character_trips() {
        let watch = DegenerateWatch::default();
        assert!(watch.feed(&"!".repeat(RUN_LIMIT as usize)));
        assert!(watch.is_tripped());
    }

    #[test]
    fn a_run_split_across_fragments_still_trips() {
        // The fault seen came in 8KB fragments. A check that looked at one
        // fragment alone would have missed it on a gateway that cut the
        // stream into smaller pieces.
        let watch = DegenerateWatch::default();
        let half = RUN_LIMIT as usize / 2;
        assert!(!watch.feed(&"!".repeat(half)));
        assert!(watch.feed(&"!".repeat(half)));
    }

    #[test]
    fn one_character_short_of_the_limit_does_not_trip() {
        let watch = DegenerateWatch::default();
        assert!(!watch.feed(&"!".repeat(RUN_LIMIT as usize - 1)));
        assert!(!watch.is_tripped());
    }

    #[test]
    fn a_different_character_resets_the_run() {
        // The control the guard lives or dies by. A long run broken by any
        // other character is normal text, odd as it looks. It has to run as
        // long as it likes.
        let watch = DegenerateWatch::default();
        let near = "-".repeat(RUN_LIMIT as usize - 1);
        for _ in 0..8 {
            assert!(!watch.feed(&near));
            assert!(!watch.feed("x"));
        }
        assert!(!watch.is_tripped());
    }

    #[test]
    fn ordinary_prose_never_trips() {
        let watch = DegenerateWatch::default();
        let prose = "The user asks a simple math question: 2+2. \
                     They want a brief answer.\n\n";
        for _ in 0..500 {
            assert!(!watch.feed(prose));
        }
        assert!(!watch.is_tripped());
    }

    #[test]
    fn a_tripped_stream_reports_once() {
        let watch = DegenerateWatch::default();
        assert!(watch.feed(&"!".repeat(RUN_LIMIT as usize)));
        assert!(!watch.feed(&"!".repeat(RUN_LIMIT as usize)));
    }

    #[test]
    fn multibyte_characters_are_counted_as_characters_not_bytes() {
        // A count of bytes would trip three times early on a 3-byte
        // character. That is how a guard sized on English text ends up
        // firing on text that is not English.
        let watch = DegenerateWatch::default();
        assert!(!watch.feed(&"の".repeat(RUN_LIMIT as usize - 1)));
        assert!(watch.feed("の"));
    }

    #[test]
    fn a_reset_drops_the_run_carried_from_the_last_attempt() {
        // The retry control. One attempt ends deep into a run of dashes. The
        // next opens on more of them. Without the reset the two add up and
        // the fresh stream is cut for what the last one wrote.
        let watch = DegenerateWatch::default();
        assert!(!watch.feed(&"-".repeat(RUN_LIMIT as usize - 1)));
        watch.reset();
        assert!(!watch.feed(&"-".repeat(RUN_LIMIT as usize - 1)));
        assert!(!watch.is_tripped());
    }

    #[test]
    fn a_reset_clears_the_mark() {
        let watch = DegenerateWatch::default();
        assert!(watch.feed(&"!".repeat(RUN_LIMIT as usize)));
        watch.reset();
        assert!(!watch.is_tripped());
        assert!(watch.feed(&"!".repeat(RUN_LIMIT as usize)));
    }

    #[tokio::test]
    async fn a_reset_drops_a_wake_the_last_attempt_never_consumed() {
        // The retry control for the wake, beside the one for the count. A
        // trip with no waiter present saves a permit. If `reset` left it
        // there, the next attempt's waiter would take it and end a stream
        // that had written nothing wrong.
        use futures_util::FutureExt;

        let watch = DegenerateWatch::default();
        assert!(watch.feed(&"!".repeat(RUN_LIMIT as usize)));
        watch.reset();
        assert!(
            watch.tripped().now_or_never().is_none(),
            "a fresh attempt's waiter must not be woken by the last attempt's trip"
        );
    }

    #[tokio::test]
    async fn awaiting_a_trip_that_already_happened_returns_at_once() {
        let watch = DegenerateWatch::default();
        watch.feed(&"!".repeat(RUN_LIMIT as usize));
        watch.tripped().await;
    }

    #[tokio::test]
    async fn awaiting_wakes_on_a_trip_from_another_task() {
        use std::sync::Arc;

        let watch = Arc::new(DegenerateWatch::default());
        let feeder = Arc::clone(&watch);
        tokio::spawn(async move {
            feeder.feed(&"!".repeat(RUN_LIMIT as usize));
        });
        watch.tripped().await;
        assert!(watch.is_tripped());
    }
}
