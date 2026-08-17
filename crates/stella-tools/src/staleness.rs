//! Content identity for the file tools: one hash of what a file held, so a
//! later read can tell "unchanged" from "someone else moved this".
//!
//! Two callers, both in this crate. [`hex_sha256`] is the read→edit drift
//! oracle's baseline (#331): [`crate::read`] records it for the bytes the
//! model saw, and [`crate::edit`] compares it against current disk bytes to
//! attribute a failed match to an out-of-band change rather than blaming the
//! model's needle. `sha256_8` (crate-private) is the short stamp a mutating
//! tool puts in its success output so distinct effects render as distinct
//! bytes (#3176).
//!
//! Git hashes are deliberately NOT used for this: content hashing works with
//! a dirty working tree and with files git never sees, which is the normal
//! state of a workspace mid-turn.
//!
//! This module once also carried a per-file freshness manifest for the
//! `gather_context` / `save_exploration` artifact cache. Those tools are gone
//! and nothing replaced that consumer, so the manifest half was removed with
//! them rather than left as unwired code behind a spec citation that no
//! longer resolves.

use sha2::{Digest, Sha256};

/// Hex-encoded SHA-256 of raw bytes — the manifest entry format.
pub fn hex_sha256(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for b in digest {
        // `write!` into the pre-sized buffer: this runs once per file on
        // every freshness check and once per `read_file`, and the obvious
        // `push_str(&format!(…))` allocates a String per BYTE of digest.
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// First 8 hex chars of [`hex_sha256`] — the short content identity a
/// mutating tool stamps into its success output so distinct effects render
/// distinct bytes. The engine's stagnation detector keys on byte-identical
/// tool output; a constant success string made seven different edits look
/// like a stuck loop and killed a correct in-progress solve (#3176). Eight
/// chars is plenty: the digest only has to separate the handful of outputs a
/// detector window compares, not resist collision attacks.
pub(crate) fn sha256_8(bytes: &[u8]) -> String {
    let mut hex = hex_sha256(bytes);
    hex.truncate(8);
    hex
}
