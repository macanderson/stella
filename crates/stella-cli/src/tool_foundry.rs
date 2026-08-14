//! Tool Foundry wiring — the CLI half of the self-authored-tool protocol.
//!
//! A proposal is a manifest+script pair staged under `.stella/tools/proposed/`,
//! a directory discovery's non-recursive scan can never register from. The
//! verbs live in [`adopt`]: `stella tools --adopt` proves a staged tool with
//! its capability witness, `--enable` is the one approval a machine never
//! grants itself, and `--foundry` reports every self-authored tool with its
//! proof and reuse record.

pub(crate) mod adopt;

/// A tiny FNV-1a hash — enough to key dedup deterministically, and it keeps
/// this module free of a hashing dependency. `pub(crate)` because the ingest
/// staleness alerts (`ingest_cmd::lineage`) derive their notification ids
/// this way, and two copies of a hash function is how they drift apart.
pub(crate) fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash keys durable notification ids (`ingest_cmd::lineage`), so its
    /// output for a given input is a compatibility surface: a changed value
    /// re-surfaces every already-delivered notification.
    #[test]
    fn fnv1a_is_stable_for_a_signature() {
        assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a("jq {p1} {p2}"), fnv1a("jq {p1} {p2}"));
        assert_ne!(fnv1a("jq {p1} {p2}"), fnv1a("jq {p1}"));
    }
}
