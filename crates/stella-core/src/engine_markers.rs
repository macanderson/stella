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
//! # The tie, in every direction
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
//! That sentence is a check rather than an instruction:
//! `the_table_holds_every_marker_shaped_constant` scans the sources that own
//! an entry for marker-shaped constants and fails, by name, on one that opens
//! a bracketed tag and is not in the table. An instruction in a doc comment is
//! the convention-only coupling this repository removed for prompt contracts.
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

    /// Every source file that owns an [`ENGINE_MARKERS`] entry today.
    ///
    /// A file that stops owning one fails the scan below by name rather than
    /// dropping out of it silently, which is what makes this list safe to keep
    /// by hand.
    const MARKER_SOURCES: &[(&str, &str)] = &[
        ("driver.rs", include_str!("driver.rs")),
        ("driver/truncation.rs", include_str!("driver/truncation.rs")),
        ("driver/user_hooks.rs", include_str!("driver/user_hooks.rs")),
        (
            "driver/deadline_notice.rs",
            include_str!("driver/deadline_notice.rs"),
        ),
        ("receipts.rs", include_str!("receipts.rs")),
        ("restore.rs", include_str!("restore.rs")),
        ("waiting.rs", include_str!("waiting.rs")),
        ("skill_invocation.rs", include_str!("skill_invocation.rs")),
        (
            "compaction/read_digest.rs",
            include_str!("compaction/read_digest.rs"),
        ),
    ];

    /// Marker-shaped constants that are not [`ENGINE_MARKERS`] entries, with
    /// the reason each one is out.
    ///
    /// The table is for `User`-role message *openings*. A constant that marks
    /// something else is not a candidate, and saying so here is what stops the
    /// scan's rule accreting one.
    const NOT_A_USER_ROLE_OPENING: &[(&str, &str)] = &[(
        "FILE_CAP_MARKER",
        "marks a truncation point inside restored file content, not the \
         opening of a message",
    )];

    /// Whether a constant's name is marker-shaped.
    ///
    /// `REASONING_ONLY_PARTIAL` is the shape this rule must not admit: it is a
    /// bracketed assistant-role stand-in, and its name carries neither token.
    fn marker_shaped(name: &str) -> bool {
        name.contains("MARKER") || name.ends_with("_PREFIX")
    }

    /// The string literal a `const NAME: &str = "…"` declaration opens with,
    /// or `None` when the declaration has no literal to read.
    ///
    /// Reads past the end of the line, because a long literal is written on
    /// the next one.
    fn first_literal(after_equals: &str) -> Option<String> {
        let open = after_equals.find('"')?;
        let rest = &after_equals[open + 1..];
        let mut literal = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    literal.push(c);
                    literal.push(chars.next()?);
                }
                '"' => return Some(literal),
                _ => literal.push(c),
            }
        }
        None
    }

    /// Every marker-shaped constant one source file declares, as
    /// `(name, literal)`.
    fn marker_constants(source: &str) -> Vec<(String, String)> {
        let mut found = Vec::new();
        for (offset, _) in source.match_indices("const ") {
            let rest = &source[offset + "const ".len()..];
            let Some(colon) = rest.find(':') else {
                continue;
            };
            let name = rest[..colon].trim();
            if name.is_empty() || !marker_shaped(name) {
                continue;
            }
            let after_colon = &rest[colon + 1..];
            // Only `&str` constants; a `&[&str]` table is not a marker.
            if !after_colon.trim_start().starts_with("&str") {
                continue;
            }
            let Some(equals) = after_colon.find('=') else {
                continue;
            };
            if let Some(literal) = first_literal(&after_colon[equals + 1..]) {
                found.push((name.to_string(), literal));
            }
        }
        found
    }

    /// **Witness.** A marker-shaped constant that opens a bracketed tag and
    /// never joins [`ENGINE_MARKERS`] fails here, by name.
    ///
    /// This is the third coupling direction. Table→prompt is enforced by
    /// `stella-cli`'s prompt parity suite and table→classifier by
    /// `receipts::tests`; nothing else asks whether the *constants* are all in
    /// the table, and a doc comment saying to add one is not a check.
    ///
    /// Add `pub(crate) const FAKE_STEER_PREFIX: &str = "[fake steer";` to
    /// `driver/truncation.rs` and this fails naming it, until it joins the
    /// table — whereupon the receipts and prompt couplings fail in turn, which
    /// is the designed cascade.
    #[test]
    fn the_table_holds_every_marker_shaped_constant() {
        let mut scanned: Vec<(&str, String, String)> = Vec::new();
        for (path, source) in MARKER_SOURCES {
            let found = marker_constants(source);
            assert!(
                !found.is_empty(),
                "{path} declares no marker-shaped constant, so the scan reads \
                 it for nothing — the marker moved and this list is stale"
            );
            for (name, literal) in found {
                scanned.push((path, name, literal));
            }
        }

        for (path, name, literal) in &scanned {
            if NOT_A_USER_ROLE_OPENING
                .iter()
                .any(|(excluded, _)| excluded == name)
            {
                continue;
            }
            if !literal.starts_with('[') {
                continue;
            }
            assert!(
                ENGINE_MARKERS.contains(&literal.as_str()),
                "{path}'s {name} opens a bracketed User-role marker that \
                 ENGINE_MARKERS does not list. Add it to the table in this \
                 change, or record it in NOT_A_USER_ROLE_OPENING with the \
                 reason it is not one."
            );
        }

        for marker in ENGINE_MARKERS {
            assert!(
                scanned.iter().any(|(_, _, literal)| literal == marker),
                "no scanned source declares {marker:?}, so MARKER_SOURCES no \
                 longer covers every file that owns a table entry"
            );
        }
    }

    /// Anti-vacuity for the scan itself: a parser that recognised nothing
    /// would pass every assertion above.
    #[test]
    fn the_marker_scan_reads_the_shapes_it_claims_to() {
        let sample = "pub(crate) const A_MARKER_PREFIX: &str = \"[a marker\";\n\
                      const PLAIN: &str = \"[not marker shaped\";\n\
                      const B_MARKER: &str =\n    \"[wrapped onto the next line\";\n\
                      const A_TABLE: &[&str] = &[\"[not a single marker\"];\n";
        let found = marker_constants(sample);
        assert_eq!(
            found,
            vec![
                ("A_MARKER_PREFIX".to_string(), "[a marker".to_string()),
                (
                    "B_MARKER".to_string(),
                    "[wrapped onto the next line".to_string()
                ),
            ],
            "the scan must read a wrapped literal, skip a name that is not \
             marker-shaped, and skip a table of them"
        );
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
