//! Production implementations of `stella-core`'s time ports. The engine
//! exports only the [`Clock`] and [`Sleeper`] traits (ports, not
//! concretions), so the binary owns the concrete
//! wall-clock/tokio impls and wires them at construction. This is what
//! keeps `stella-core` free of production `tokio::time` calls.

use async_trait::async_trait;
use stella_core::ports::Clock;
use stella_core::retry::Sleeper;

/// The production clock: monotonic milliseconds since construction.
pub struct SystemClock {
    origin: std::time::Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: std::time::Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }
}

/// A [`Clock`] anchored to the **wall** (Unix-epoch milliseconds), for
/// stamps that must stay comparable across processes and runs — the fleet
/// ledger's rows, where issue #1222's cache-warmth projection compares a
/// prior run's `finished_at_ms` against "now". [`SystemClock`]'s
/// per-construction origin is the right shape for in-process elapsed
/// arithmetic (retry pacing, watch caps) and exactly the wrong one for a
/// durable timestamp: two runs' stamps share no origin, so their difference
/// means nothing. Not strictly monotonic (NTP can step it), which is why it
/// does not replace [`SystemClock`] everywhere; a pre-epoch system clock
/// reads as `0` rather than panicking.
#[derive(Debug, Default, Clone, Copy)]
pub struct WallClock;

impl Clock for WallClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// The production [`Sleeper`]: a thin wrapper over `tokio::time::sleep`.
#[derive(Debug, Default, Clone, Copy)]
pub struct TokioSleeper;

#[async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration_ms: u64) {
        tokio::time::sleep(std::time::Duration::from_millis(duration_ms)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_starts_near_zero_and_advances_monotonically() {
        let clock = SystemClock::new();
        let first = clock.now_ms();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = clock.now_ms();
        assert!(second >= first, "clock must never go backwards");
        assert!(
            second - first >= 4,
            "clock must actually advance with wall time"
        );
    }

    #[test]
    fn default_constructs_a_fresh_clock() {
        let clock = SystemClock::default();
        assert!(
            clock.now_ms() < 1000,
            "a freshly constructed clock starts near zero"
        );
    }

    #[test]
    fn wall_clock_reads_epoch_milliseconds_not_a_process_origin() {
        let now = WallClock.now_ms();
        // Any plausible present is decades past the epoch — a process-origin
        // clock would read near zero here, which is exactly the bug this
        // clock exists to avoid for durable stamps.
        assert!(
            now > 1_600_000_000_000,
            "wall clock must be epoch-anchored, got {now}"
        );
    }
}
