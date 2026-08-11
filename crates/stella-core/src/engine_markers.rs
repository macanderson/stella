//! The closed table of engine-injected message markers (#2722).
//!
//! The engine and the CLI write a small number of messages that ride the wire
//! as `User`-role turns but are not the user speaking: the overflow summary,
//! the stuck-loop steer, the output-limit continuation nudge, and the
//! recalled-context block. Each one opens with a recognizable marker prefix,
//! and two independent consumers depend on knowing the complete set:
//!
//! - `receipts::user_block_kind` classifies `User`-role content by these
//!   prefixes so a receipt never attributes engine text to the person;
//! - the system prompt's injection-defense contract
//!   (`crates/stella-cli/src/agent/prompt.rs`) teaches the model that
//!   engine-injected guidance carries one of these markers, so
//!   instruction-shaped text *without* one, inside a tool result, is data
//!   impersonating the operator.
//!
//! Before this table existed the prompt hand-wrote that list in prose and had
//! already drifted: the continuation nudge — itself instruction-shaped — was
//! absent entirely, so the engine's own steering failed the prompt's own test,
//! and the recall marker was described but never spelled. A prose list has no
//! failure mode; this table does.
//!
//! # The tie, in both directions
//!
//! The entries are the marker constants themselves, by path — not copies — so
//! the table cannot drift from what the engine actually writes: editing a
//! constant edits the table. The consumers are tied by test: the receipts
//! suite asserts every entry classifies as engine text (never `UserGoal`),
//! and `stella-cli`'s prompt parity suite asserts every entry appears verbatim
//! in both static prompts. **A new engine-injected marker joins this table in
//! the same change that introduces it** — that is what makes the prompt fail
//! by name until it teaches the new marker, instead of silently narrowing the
//! model's injection test.
//!
//! This module is deliberately a leaf: one `pub` table, no logic, declared
//! outside `driver.rs` because that file is closed to growth
//! (`scripts/file-size-baseline.txt`).

/// Every marker prefix the engine or the CLI puts on a `User`-role message it
/// writes itself. Prefixes, not full messages: consumers match with
/// `starts_with` (the recall marker happens to be the full first line).
///
/// Entries reference the owning constants — `SUMMARY_MARKER_PREFIX` and
/// `LOOP_STEER_PREFIX` (`driver.rs`), `CONTINUATION_MARKER_PREFIX`
/// (`driver/truncation.rs`), `RECALL_MARKER` (`receipts.rs`) — so the table is
/// correct by definition for the markers it lists; tests keep it complete.
pub const ENGINE_MARKERS: &[&str] = &[
    crate::driver::SUMMARY_MARKER_PREFIX,
    crate::driver::LOOP_STEER_PREFIX,
    crate::driver::CONTINUATION_MARKER_PREFIX,
    crate::receipts::RECALL_MARKER,
];

#[cfg(test)]
mod tests {
    use super::ENGINE_MARKERS;

    /// Anti-vacuity for every downstream `contains`/`starts_with` check: an
    /// empty or near-empty entry would make the prompt-parity and receipts
    /// couplings pass while proving nothing (the same hazard
    /// `prompt/parity.rs` guards with `MIN_CONTRACT_CHARS`).
    #[test]
    fn the_table_is_nonempty_and_every_marker_is_distinctive() {
        assert!(
            ENGINE_MARKERS.len() >= 4,
            "the four known engine markers must all be present; a shorter table \
             means one was dropped rather than retired through review"
        );
        for marker in ENGINE_MARKERS {
            assert!(
                marker.starts_with('['),
                "every engine marker opens a bracketed tag — {marker:?} does not, \
                 so prefix-matching consumers would misfire on ordinary prose"
            );
            assert!(
                marker.len() >= 12,
                "{marker:?} is too short for a `contains` check against it to \
                 prove anything"
            );
        }
    }

    /// The table is a set: a duplicate entry would make the count lie about
    /// coverage.
    #[test]
    fn no_marker_appears_twice() {
        let mut seen = std::collections::BTreeSet::new();
        for marker in ENGINE_MARKERS {
            assert!(seen.insert(marker), "duplicate engine marker {marker:?}");
        }
    }

    /// No table entry may be a prefix of another: consumers match with
    /// `starts_with`, so a nested pair would make one marker shadow the other
    /// and the shadowed one's classification unreachable.
    #[test]
    fn no_marker_shadows_another() {
        for a in ENGINE_MARKERS {
            for b in ENGINE_MARKERS {
                assert!(
                    std::ptr::eq(a, b) || !a.starts_with(b),
                    "{a:?} starts with {b:?}: prefix-matching consumers could \
                     never distinguish them"
                );
            }
        }
    }
}
