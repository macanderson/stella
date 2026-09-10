// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The real time sources behind `stella-core`'s two time ports.
//!
//! [`stella_core::retry::Sleeper`] and [`stella_core::ports::Clock`] are
//! traits. The engine reads time, waits and times out only through them. A
//! real run gets its answers here: a sleeper on the Tokio timer, a clock
//! counting from the Unix epoch, and one counting from the first read. The
//! README says why the three share one crate below every host, and
//! `test_util` holds the two doubles every test against the engine shares.

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use rand::RngExt;
use stella_core::ports::Clock;
use stella_core::retry::Sleeper;

#[cfg(feature = "test-util")]
pub mod test_util;

/// The real [`Sleeper`]. It waits on `tokio::time::sleep`, reads `now` off
/// the monotonic clock, and draws its jitter from the OS entropy pool, which
/// spreads retriers across the backoff window.
///
/// It needs a Tokio runtime with its time driver on. Every host here builds
/// one.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TokioSleeper;

#[async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration_ms: u64) {
        tokio::time::sleep(std::time::Duration::from_millis(duration_ms)).await;
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn jitter(&self, upper: u64) -> u64 {
        rand::rng().random_range(0..=upper)
    }
}

/// A [`Clock`] counting milliseconds from the Unix epoch.
///
/// For a stamp read by another process or a later run: a hook event on the
/// far side of a socket, a fleet ledger row, a verdict stamp. Two such
/// stamps must count from one start. The epoch is the only start every
/// process agrees on.
///
/// Not for a deadline. The system clock can be stepped, by NTP or by hand.
/// A deadline set against it moves with it. The engine's deadlines come off
/// [`Sleeper::now`] instead.
///
/// A system clock set before the epoch reads as `0`. One past what `u64`
/// holds reads as `u64::MAX`. A wrong time makes a bad stamp. A run that
/// stops for one is worse.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WallClock;

impl Clock for WallClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            })
    }
}

/// The origin every [`MonotonicClock`] counts from: the first read in this
/// process. One cell, so two instances built at different moments agree.
static ORIGIN: OnceLock<Instant> = OnceLock::new();

/// A [`Clock`] counting milliseconds from the process's first read of it.
/// It never goes backwards.
///
/// For a span measured inside one process and compared as a number: a
/// circuit breaker's cooling window, a CI monitor's wall cap. Every
/// instance shares the origin, so two holders built at different moments
/// read one timeline. A clock whose origin was the moment it was built
/// would put their readings apart, by the time between the two builds, and
/// say nothing.
///
/// The first read in the process reads `0`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MonotonicClock;

impl Clock for MonotonicClock {
    fn now_ms(&self) -> u64 {
        let origin = ORIGIN.get_or_init(Instant::now);
        u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_monotonic_clock_never_goes_backwards() {
        let first = MonotonicClock.now_ms();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = MonotonicClock.now_ms();
        assert!(second >= first, "clock must never go backwards");
        assert!(
            second - first >= 4,
            "5ms of sleep should register: {first} -> {second}"
        );
    }

    /// Two instances share one origin, so a reading taken on one is a
    /// reading on the other.
    #[test]
    fn every_monotonic_clock_shares_one_origin() {
        let first = MonotonicClock;
        let mark = first.now_ms() + 10_000;
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = MonotonicClock;
        let now = second.now_ms();
        assert!(
            now < mark,
            "a fresh instance reads the same clock: {now} vs {mark}"
        );
        assert!(
            mark - now <= 10_000,
            "and it has moved on from the first read"
        );
    }

    #[test]
    fn the_wall_clock_counts_from_the_unix_epoch() {
        // 2026-01-01T00:00:00Z. The test runs after that date. The point is
        // the origin, not the exact reading.
        let epoch_2026_ms: u64 = 1_767_225_600_000;
        assert!(WallClock.now_ms() > epoch_2026_ms);
    }

    #[test]
    fn jitter_stays_inside_the_window() {
        for _ in 0..1_000 {
            assert!(TokioSleeper.jitter(7) <= 7);
        }
        assert_eq!(TokioSleeper.jitter(0), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_sleeper_waits_on_the_tokio_timer() {
        let before = tokio::time::Instant::now();
        TokioSleeper.sleep(250).await;
        assert!(before.elapsed() >= std::time::Duration::from_millis(250));
    }

    #[test]
    fn the_sleepers_now_is_the_monotonic_clock() {
        let a = TokioSleeper.now();
        let b = TokioSleeper.now();
        assert!(b >= a);
    }
}
