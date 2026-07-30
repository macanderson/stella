// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Compile-time build identity — everything `-V` and `--version` report.
//!
//! One module because the three surfaces are one contract: `build.rs` stamps a
//! single `STELLA_BUILD_VERSION` literal, `-V` prints its interior, `--version`
//! extends it with the build target and profile, and the claim launcher attests
//! the NUL-delimited bytes without executing the binary. Keeping them adjacent
//! is what makes "the two surfaces cannot disagree about the version" checkable
//! by reading one file.

/// NUL boundaries prevent LLVM's string pooling from adjoining identifier
/// characters to the version bytes. The claim launcher can therefore attest
/// the full compile-time identity without executing the binary.
pub(crate) const BUILD_VERSION_IDENTITY: &str = concat!("\0", env!("STELLA_BUILD_VERSION"), "\0");

/// The version string shown by `--version` and `stella version`. `build.rs`
/// turns an optional compile-time `STELLA_BUILD_GIT_SHA` into one contiguous
/// literal; this returns the interior of the deliberately delimited identity.
/// Ordinary release builds still carry the bare package version.
pub(crate) fn version_string() -> &'static str {
    &BUILD_VERSION_IDENTITY[1..BUILD_VERSION_IDENTITY.len() - 1]
}

/// clap's `version` attribute needs a `'static` string.
pub(crate) fn version_static() -> &'static str {
    version_string()
}

/// What `--version` prints, as opposed to `-V`'s single line.
///
/// clap's convention, which `cargo`/`rustc` both follow: the short flag stays
/// greppable and the long one answers the questions a bug report otherwise
/// costs a round trip — which machine this binary was built for, and whether
/// it is an optimized build. Composed with `concat!` so it is one `'static`
/// literal with no allocation and no startup work.
///
/// Deliberately does NOT restate [`version_string`] through a second code
/// path: it interpolates the same `STELLA_BUILD_VERSION` literal `build.rs`
/// stamped, so the two surfaces cannot disagree about the version itself.
pub(crate) fn long_version_static() -> &'static str {
    concat!(
        env!("STELLA_BUILD_VERSION"),
        "\ntarget:  ",
        env!("STELLA_BUILD_TARGET"),
        "\nprofile: ",
        env!("STELLA_BUILD_PROFILE"),
    )
}
