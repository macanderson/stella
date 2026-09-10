// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The two [`Sleeper`] doubles every test against the engine shares.
//!
//! Behind the `test-util` feature, so a release build carries neither. A
//! crate that tests the engine takes this crate as a dev-dependency with the
//! feature on, the way tokio's tests take `tokio-test`.

use std::time::Instant;

use async_trait::async_trait;
use stella_core::retry::Sleeper;

/// A [`Sleeper`] on tokio's clock, for a test that runs its runtime paused.
///
/// A sleep costs nothing while the runtime is idle. It still lets a pending
/// call finish first, and `now` reads the same virtual timeline. That is
/// what a timeout is. So this is the double for any test that arms a tool
/// or model timeout. A sleeper that returned at once would fire every
/// engine timeout as soon as a provider future waited on another task.
///
/// Jitter is zero: a test that asserts on retry timing wants no spread.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PausedSleeper;

#[async_trait]
impl Sleeper for PausedSleeper {
    async fn sleep(&self, duration_ms: u64) {
        tokio::time::sleep(std::time::Duration::from_millis(duration_ms)).await;
    }

    fn now(&self) -> Instant {
        tokio::time::Instant::now().into_std()
    }

    fn jitter(&self, _upper: u64) -> u64 {
        0
    }
}

/// A [`Sleeper`] that never waits.
///
/// For a test that arms no timeout and wants a retry ladder to run at once.
/// `now` reads the real monotonic clock, so a deadline set from it still
/// means what it says. Do not drive a timed turn with it: see
/// [`PausedSleeper`] for why.
///
/// Jitter is zero, for the same reason as [`PausedSleeper`]'s.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NoopSleeper;

#[async_trait]
impl Sleeper for NoopSleeper {
    async fn sleep(&self, _duration_ms: u64) {}

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn jitter(&self, _upper: u64) -> u64 {
        0
    }
}
