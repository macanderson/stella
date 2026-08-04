//! Test-only counters for what a recall *cost*, so the guards in
//! [`crate::retrieval`] can assert a complexity and I/O class instead of a wall
//! clock. Compiled out entirely in a release build.
//!
//! # Why thread-local, and why they get merged
//!
//! A process-global counter is shared with every other test in the binary, and
//! `cargo test` runs them concurrently — so a budget assertion over one would
//! measure whatever else happened to be loading nodes at the same moment, and
//! could only be made to pass by loosening it until it proved nothing.
//!
//! But recall's blocking pass runs on the blocking pool, not on the thread that
//! awaited it, so a thread-local read by the test would see **zero** and every
//! guard would pass vacuously — strictly worse than a loose global. So the
//! blocking closure [`take`]s its own thread's tally and the awaiting side
//! [`merge`]s it back in. The count a test reads is then exactly the work its own
//! recall did, wherever the runtime chose to run it.

use std::cell::Cell;

thread_local! {
    /// Content bytes pulled across the SQLite boundary on this thread.
    static CONTENT_BYTES: Cell<u64> = const { Cell::new(0) };
    /// Cosine evaluations performed on this thread.
    static COSINES: Cell<usize> = const { Cell::new(0) };
}

/// One thread's tally, in transit between the blocking pool and its awaiter.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Counts {
    /// Content bytes read out of SQLite.
    pub(crate) content_bytes: u64,
    /// Cosine evaluations performed.
    pub(crate) cosines: usize,
}

/// Record content bytes crossing the SQLite boundary.
pub(crate) fn add_content_bytes(len: usize) {
    CONTENT_BYTES.with(|c| c.set(c.get() + len as u64));
}

/// Record one cosine evaluation.
pub(crate) fn add_cosine() {
    COSINES.with(|c| c.set(c.get() + 1));
}

/// Zero this thread's tally and return it — called on the blocking thread once
/// the pass is done.
pub(crate) fn take() -> Counts {
    Counts {
        content_bytes: CONTENT_BYTES.with(|c| c.replace(0)),
        cosines: COSINES.with(|c| c.replace(0)),
    }
}

/// Fold a tally carried back from another thread into this one's.
pub(crate) fn merge(counts: Counts) {
    CONTENT_BYTES.with(|c| c.set(c.get() + counts.content_bytes));
    COSINES.with(|c| c.set(c.get() + counts.cosines));
}

/// Zero this thread's tally and return the content bytes it held.
pub(crate) fn take_content_bytes_loaded() -> u64 {
    take().content_bytes
}

/// Zero this thread's tally and return the cosine count it held.
pub(crate) fn take_cosine_calls() -> usize {
    take().cosines
}
