// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Property 1 of `docs/spec/diagnostics.md` §12: **content cannot enter a
//! record.**
//!
//! This is the witness for §5.2, and it is a stronger artifact than a sentinel
//! sweep because it fails at the only moment that matters — before the code
//! that would have leaked exists. A sweep tests the sentinel you wrote, not the
//! property you meant, which is the lesson `serve-observability.md` §8 records
//! after a working vacuity guard still missed a planted leak.
//!
//! A runtime test cannot observe a compile error, so this shells out to the
//! compiler and diffs its output. That the expected stderr is stable at all is
//! a consequence of `rust-toolchain.toml` pinning a concrete Rust version:
//! floating on `stable` would rewrite these files on every release, which is
//! the same reason AGENTS.md gives for pinning rustfmt.
//!
//! Regenerate after an intentional change (including a toolchain bump) with:
//!
//! ```text
//! TRYBUILD=overwrite cargo test -p stella-diag --test compile_fail
//! ```

/// Each case is a way someone would *naturally* try to log runtime content.
/// None of them is exotic; every one is a line a reasonable contributor writes
/// on a Tuesday, and every one is a privacy incident under `tracing`'s `%value`.
#[test]
fn content_in_a_log_does_not_compile() {
    trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}
