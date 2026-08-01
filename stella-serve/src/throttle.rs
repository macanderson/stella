// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The 401 throttle: a dependency-free token bucket that slows a sustained
//! token guess without telling the guesser it has been noticed.
//!
//! Split out of `server.rs` because it is self-contained policy with its own
//! tests, and that file is at the size ratchet's limit. Nothing here touches
//! the server's state; `route` consults it and holds the response.

use std::time::Duration;

/// Burst of 401s answered with no delay. A host that restarts and races a few
/// requests against a not-yet-updated token, or a health probe that forgets the
/// header, should not be punished for the first handful.
pub(crate) const UNAUTHORIZED_BURST: f64 = 8.0;

/// Steady-state 401s per second once the burst is spent.
pub(crate) const UNAUTHORIZED_REFILL_PER_SEC: f64 = 2.0;

/// Delay applied to a 401 that arrives with the bucket empty.
///
/// This is deliberately a *delay*, not a rejection: a 429 or a dropped
/// connection would tell an attacker they had been noticed and would break a
/// legitimate client that is merely misconfigured. Holding the response instead
/// costs the guesser wall-clock time per attempt — which is the entire point,
/// since the token is a fixed shared secret with no lockout behind it — while a
/// correctly-configured host never reaches this path at all.
pub(crate) const UNAUTHORIZED_PENALTY: Duration = Duration::from_millis(500);

/// A dependency-free token bucket. Deliberately per-process and not per-peer:
/// tracking source addresses would mean unbounded state keyed by something the
/// caller chooses, which is its own denial-of-service surface, and this is a
/// sidecar for exactly one trusted host — a legitimate deployment produces no
/// sustained 401s at all, so a global bucket costs it nothing.
pub(crate) struct TokenBucket {
    tokens: f64,
    last: std::time::Instant,
}

impl TokenBucket {
    pub(crate) fn new() -> Self {
        Self {
            tokens: UNAUTHORIZED_BURST,
            last: std::time::Instant::now(),
        }
    }

    /// Spend one token, returning the delay the caller must observe first.
    pub(crate) fn take(&mut self, now: std::time::Instant) -> Duration {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * UNAUTHORIZED_REFILL_PER_SEC).min(UNAUTHORIZED_BURST);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Duration::ZERO
        } else {
            UNAUTHORIZED_PENALTY
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A brute-force run against the only credential this service has must
    /// stop being free after the burst. Driven over an injected `Instant` so
    /// the refill is asserted exactly, with no sleeping in the test.
    #[test]
    fn sustained_401s_are_throttled_once_the_burst_is_spent() {
        let start = std::time::Instant::now();
        let mut bucket = TokenBucket::new();
        for attempt in 0..UNAUTHORIZED_BURST as usize {
            assert_eq!(
                bucket.take(start),
                Duration::ZERO,
                "attempt {attempt} is within the burst and must not be delayed"
            );
        }
        assert_eq!(
            bucket.take(start),
            UNAUTHORIZED_PENALTY,
            "the first attempt past the burst is held"
        );
        // Half a second of refill at 2/sec is exactly one token.
        assert_eq!(
            bucket.take(start + Duration::from_millis(500)),
            Duration::ZERO
        );
        assert_eq!(
            bucket.take(start + Duration::from_millis(500)),
            UNAUTHORIZED_PENALTY,
            "and it is spent again immediately"
        );
    }

    #[test]
    fn refill_is_capped_at_the_burst_size() {
        let start = std::time::Instant::now();
        let mut bucket = TokenBucket::new();
        // An hour idle must not bank an hour of tokens.
        let later = start + Duration::from_secs(3600);
        for _ in 0..UNAUTHORIZED_BURST as usize {
            assert_eq!(bucket.take(later), Duration::ZERO);
        }
        assert_eq!(bucket.take(later), UNAUTHORIZED_PENALTY);
    }
}
