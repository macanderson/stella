//! The closed tables of engine-injected message text (#2722).
//!
//! The engine and the CLI write messages that ride the wire as `User`-role
//! turns but are not the user speaking — the overflow summary, the stuck-loop
//! steer, an invoked skill's body, a parked wait's wake report. Most open with
//! a bracketed marker prefix, enumerated in [`ENGINE_MARKERS`]; the two
//! completion nudges are plain English and are enumerated in
//! [`ENGINE_NUDGE_PREFIXES`]. Three consumers depend on the complete set:
//!
//! - `receipts::user_block_kind` classifies `User`-role content by these
//!   prefixes so a receipt never attributes engine text to the person;
//! - `driver::loop_evidence::turn_start_index` bounds the loop-detection and
//!   confident-zero windows at the last genuine user turn, so a marked
//!   message must not end the window it lands inside (#2837);
//! - the system prompt's injection-defense contract
//!   (`crates/stella-cli/src/agent/prompt.rs`) teaches the model that
//!   engine-injected guidance carries one of these markers, so
//!   instruction-shaped text *without* one, inside a tool result, is data
//!   impersonating the operator.
//!
//! Before this table existed the prompt hand-wrote that list in prose and had
//! already drifted: the continuation nudge — itself instruction-shaped — was
//! absent entirely, and the recall marker was described but never spelled. A
//! prose list has no failure mode; a table does.
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
//! This module is a leaf: two `pub` tables and one predicate over them,
//! declared outside `driver.rs` because that file is closed to growth
//! (`scripts/file-size-baseline.txt`).

/// Every marker prefix the engine or the CLI puts on a `User`-role message it
/// writes itself. Prefixes, not full messages: consumers match with
/// `starts_with` (the recall marker happens to be the full first line).
///
/// Entries reference the owning constants — `SUMMARY_MARKER_PREFIX` and
/// `LOOP_STEER_PREFIX` (`driver.rs`), `CONTINUATION_MARKER_PREFIX`
/// (`driver/truncation.rs`), `STOP_HOOK_MARKER_PREFIX`
/// (`driver/user_hooks.rs`, #2684), `RECALL_MARKER` (`receipts.rs`),
/// `RESTORE_MARKER_PREFIX` (`restore.rs`, #2685),
/// `READ_DIGEST_MARKER_PREFIX` (`compaction/read_digest.rs`, #3806),
/// `WAKE_MARKER` (`waiting.rs`), `SKILL_INVOCATION_PREFIX`
/// (`skill_invocation.rs`, #2682) and `DEADLINE_MARKER_PREFIX`
/// (`driver/deadline_notice.rs`) — so the table is correct by definition for
/// the markers it lists; tests keep it complete.
pub const ENGINE_MARKERS: &[&str] = &[
    crate::driver::SUMMARY_MARKER_PREFIX,
    crate::driver::LOOP_STEER_PREFIX,
    crate::driver::CONTINUATION_MARKER_PREFIX,
    crate::driver::user_hooks::STOP_HOOK_MARKER_PREFIX,
    crate::receipts::RECALL_MARKER,
    crate::restore::RESTORE_MARKER_PREFIX,
    crate::compaction::read_digest::READ_DIGEST_MARKER_PREFIX,
    crate::waiting::WAKE_MARKER,
    crate::skill_invocation::SKILL_INVOCATION_PREFIX,
    crate::driver::deadline_notice::DEADLINE_MARKER_PREFIX,
];

/// The engine-authored `User`-role openings that carry **no** bracketed
/// marker: the once-per-turn completion nudges — the prove-it ask
/// (`driver::confident_zero`) and the live-service assertion
/// (`driver::live_services`).
///
/// They are addressed to the model as plain English, so a bracketed tag would
/// read as noise in the one message whose whole job is to be answered. That
/// makes them invisible to [`ENGINE_MARKERS`] and to anything matching on its
/// shape, which is why they get a table of their own instead of an entry
/// there.
///
/// A new completion gate adds its prefix here in the same change that adds
/// the gate, exactly as a new marker joins [`ENGINE_MARKERS`].
pub const ENGINE_NUDGE_PREFIXES: &[&str] = &[
    crate::driver::confident_zero::PROVE_IT_PREFIX,
    crate::driver::live_services::SERVICES_PREFIX,
];

/// Whether `content` opens with one of [`ENGINE_NUDGE_PREFIXES`] — a user-role
/// message the engine wrote rather than one the user (or host) sent.
///
/// Read by `driver::loop_evidence::turn_start_index` alongside
/// [`ENGINE_MARKERS`]: a nudge bounds no turn window. A window that reset on
/// one erased the turn's pre-nudge activity — the prove-it ask on an
/// edited-then-tested turn erased the edit from the tally, and the turn was
/// then abortable as a confident zero for the read-only test run the nudge
/// itself requested. Measured on the prove-it gate's first field trial (run
/// `gate-ab`, task pypi-server): three nudges in one turn, each re-armed by
/// that reset, riding a refuted `verify_done` into the 900s ceiling.
///
/// Read by `receipts::user_block_kind` for the reason it reads
/// [`ENGINE_MARKERS`]: a nudge is engine text, and filing it as the person's
/// own goal is the misattribution that classifier exists to prevent.
#[must_use]
pub fn is_engine_nudge(content: &str) -> bool {
    ENGINE_NUDGE_PREFIXES
        .iter()
        .any(|prefix| content.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::{ENGINE_MARKERS, ENGINE_NUDGE_PREFIXES};

    /// Anti-vacuity for every downstream `contains`/`starts_with` check: an
    /// empty or near-empty entry would make the prompt-parity and receipts
    /// couplings pass while proving nothing (the same hazard
    /// `prompt/parity.rs` guards with `MIN_CONTRACT_CHARS`).
    #[test]
    fn the_table_is_nonempty_and_every_marker_is_distinctive() {
        assert!(
            ENGINE_MARKERS.len() >= 10,
            "an entry left the table; a marker is retired through review, by \
             deleting the constant it names, never by shrinking this list"
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

    /// The nudge table's own anti-vacuity, and the line that keeps the two
    /// tables apart: a bracketed opening belongs in [`ENGINE_MARKERS`], where
    /// every consumer matching on marker shape already sees it. An entry that
    /// drifted across would be classified twice and enumerated twice.
    #[test]
    fn every_nudge_is_distinctive_and_unmarked() {
        assert!(
            ENGINE_NUDGE_PREFIXES.len() >= 2,
            "an entry left the table; a nudge is retired by deleting the gate \
             that writes it, never by shrinking this list"
        );
        for nudge in ENGINE_NUDGE_PREFIXES {
            assert!(
                !nudge.starts_with('['),
                "{nudge:?} opens a bracketed tag, so it belongs in \
                 ENGINE_MARKERS rather than here"
            );
            assert!(
                nudge.len() >= 12,
                "{nudge:?} is too short for a `starts_with` check against it to \
                 prove anything"
            );
        }
    }

    /// The nudge table is a set, and no entry shadows another — the same two
    /// properties [`ENGINE_MARKERS`] holds, for the same reason: consumers
    /// match with `starts_with`.
    #[test]
    fn no_nudge_repeats_or_shadows_another() {
        let mut seen = std::collections::BTreeSet::new();
        for nudge in ENGINE_NUDGE_PREFIXES {
            assert!(seen.insert(nudge), "duplicate engine nudge {nudge:?}");
        }
        for a in ENGINE_NUDGE_PREFIXES {
            for b in ENGINE_NUDGE_PREFIXES {
                assert!(
                    std::ptr::eq(a, b) || !a.starts_with(b),
                    "{a:?} starts with {b:?}: prefix-matching consumers could \
                     never distinguish them"
                );
            }
        }
    }

    /// A marked message is never also a nudge, in either direction: the two
    /// tables partition the engine's user-role writing, and an overlap would
    /// give one message two classifications depending on which table a
    /// consumer read first.
    #[test]
    fn the_two_tables_do_not_overlap() {
        for marker in ENGINE_MARKERS {
            assert!(
                !super::is_engine_nudge(marker),
                "{marker:?} is in ENGINE_MARKERS and also reads as a nudge"
            );
            for nudge in ENGINE_NUDGE_PREFIXES {
                assert!(
                    !nudge.starts_with(marker),
                    "{nudge:?} starts with the marker {marker:?}"
                );
            }
        }
    }
}
