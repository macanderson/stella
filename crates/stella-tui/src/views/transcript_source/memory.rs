// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! SPEC 6.3's two memory rows, projected — the log's four-row block and the
//! promotion's single line.
//!
//! Its own module rather than two more functions in [`super`], which sits
//! past the 1500-line ceiling already and takes no baseline entry (AGENTS.md
//! § "God files"). The seam is a real one either way: every other projection
//! there resolves *a call* — a head, a scope, a coverage, a wall time — and a
//! memory is not a call. It has no dispatch and no result, so none of the
//! `Option` fields those resolvers exist to fill apply to it.
//!
//! The ladder body and the footer are composed in
//! [`super::super::transcript`], beside every other styled row; this module
//! is the projection that hands them their arguments.

use ratatui::text::Line;
use stella_protocol::MemoryClass;

use super::super::transcript::{Event, EventKind, event_rows, memory_log_body, memory_log_footer};

/// A logged memory's whole block (SPEC 6.3): the head with the memory's id,
/// the lesson quoted, the `OBSERVATION ▸ RULE ▸ FACT` ladder with this
/// memory's rung lit, and the promotion threshold beneath.
///
/// Four rows, always. Nothing here is optional the way a head's size column
/// is, because none of it is a measurement that might not have been taken
/// yet: a memory event is emitted *after* the memory has landed, so every
/// field on it is already settled.
#[must_use]
pub fn memory_log_rows(memory: &LoggedMemory<'_>, width: usize) -> Vec<Line<'static>> {
    let mut event = Event::new(
        EventKind::MemoryLog {
            memory_id: memory.memory_id.to_string(),
        },
        "logged",
    );
    event.body = memory_log_body(
        memory.text,
        memory.class,
        memory.confidence,
        memory.kind,
        memory.decays,
    );
    event.footer = Some(memory_log_footer(memory.class, memory.promotes_at));
    event_rows(&event, width)
}

/// One logged memory, as the row needs it.
///
/// Bundled for the reason [`super::CallFacts`] is: seven of these travel
/// together into one row, and a positional list that long is a call site that
/// can pair one memory's confidence with another's threshold by ordering its
/// arguments wrongly. Borrowed rather than owned because the caller is holding
/// the fold's own [`crate::model::TranscriptEntry::MemoryLog`] and the row
/// copies only what it keeps.
pub struct LoggedMemory<'a> {
    /// The `nod_…` handle `x reject` names.
    pub memory_id: &'a str,
    /// The lesson, quoted verbatim on the row beneath the head.
    pub text: &'a str,
    /// The rung it is on — the one the ladder lights.
    pub class: MemoryClass,
    /// `0..=100`, rendered `conf 0.62`.
    pub confidence: u8,
    /// The producer's own word for the sort of memory this is.
    pub kind: &'a str,
    /// Whether its weight falls off with age.
    pub decays: bool,
    /// `0..=100`, the confidence at which it reaches the next rung.
    pub promotes_at: u8,
}

/// A promotion's one row (SPEC 6.3) — see [`EventKind::MemoryPromote`] for why
/// it is one and not a block.
#[must_use]
pub fn memory_promote_rows(
    from: MemoryClass,
    to: MemoryClass,
    confidence: u8,
    audit_event_id: &str,
    width: usize,
) -> Vec<Line<'static>> {
    event_rows(
        &Event::new(
            EventKind::MemoryPromote {
                from,
                to,
                confidence,
                audit_event_id: audit_event_id.to_string(),
            },
            "promoted",
        ),
        width,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;
    use stella_tui_theme::token;

    /// Every row of a block, joined — an assertion that a string is *absent*
    /// must not pass merely by looking at the wrong row.
    fn text_of_rows(rows: &[Line<'static>]) -> String {
        rows.iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.clone())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The rung a row lights, as `(label, foreground)` for the three ladder
    /// cells alone — the ladder is the only place these three words appear.
    fn ladder_tones(rows: &[Line<'static>]) -> Vec<(String, Option<Color>)> {
        rows.iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| matches!(span.content.as_ref(), "OBSERVATION" | "RULE" | "FACT"))
            .map(|span| (span.content.to_string(), span.style.fg))
            .collect()
    }

    /// **The witness (#5032).** A logged memory renders SPEC 6.3's block: the
    /// lesson quoted, the whole `OBSERVATION ▸ RULE ▸ FACT` ladder with **this
    /// memory's rung lit and the others receding**, its confidence, kind and
    /// decay, and the threshold it promotes at.
    ///
    /// The lit rung is asserted as a *tone*, not as text, because that is the
    /// only way to see it: every rung's word is on the row whichever one the
    /// memory is on, so a text-only assertion passes on a renderer that lights
    /// all three, none, or the wrong one. Two classes are rendered here for
    /// the same reason — a single case cannot distinguish "lights the memory's
    /// class" from "always lights the first rung".
    #[test]
    fn a_logged_memory_renders_the_ladder_with_its_own_rung_lit() {
        let observation = memory_log_rows(
            &LoggedMemory {
                memory_id: "nod_83b3f1d29a",
                text: "dedup keys must be stable across runs",
                class: MemoryClass::Observation,
                confidence: 62,
                kind: "domain",
                decays: true,
                promotes_at: 85,
            },
            120,
        );
        let text = text_of_rows(&observation);
        for want in [
            "nod_83b3f1d29a",
            "\"dedup keys must be stable across runs\"",
            "OBSERVATION ▸ RULE ▸ FACT",
            "conf 0.62",
            "kind domain",
            "decays",
            "promotes to RULE at 0.85",
            // SPEC 6.3's footer in full since #5231. `e edit` was absent while
            // it did nothing, because an affordance printed on a row that does
            // nothing when pressed is worse than an absent one.
            "e edit",
            "x reject",
        ] {
            assert!(
                text.contains(want),
                "the row does not state `{want}`: {text}"
            );
        }
        assert_eq!(
            ladder_tones(&observation),
            vec![
                ("OBSERVATION".to_string(), Some(token::TEXT)),
                ("RULE".to_string(), Some(token::DIM)),
                ("FACT".to_string(), Some(token::DIM)),
            ],
            "an observation must light OBSERVATION and only OBSERVATION"
        );

        let rule = memory_log_rows(
            &LoggedMemory {
                memory_id: "nod_1",
                text: "integration tests need the fixture server up first",
                class: MemoryClass::Rule,
                confidence: 90,
                kind: "domain",
                decays: false,
                promotes_at: 85,
            },
            120,
        );
        assert_eq!(
            ladder_tones(&rule),
            vec![
                ("OBSERVATION".to_string(), Some(token::DIM)),
                ("RULE".to_string(), Some(token::TEXT)),
                ("FACT".to_string(), Some(token::DIM)),
            ],
            "the lit rung must follow the memory's class, not the ladder's head"
        );
        let rule_text = text_of_rows(&rule);
        assert!(
            rule_text.contains("promotes to FACT at 0.85"),
            "a rule's footer names the rung above it: {rule_text}"
        );
        assert!(
            !rule_text.contains("decays"),
            "a memory that does not decay must not claim it does: {rule_text}"
        );
    }

    /// And the top of the ladder has nothing above it, so a fact states no
    /// threshold — while still offering the rejection, which is available at
    /// every rung.
    #[test]
    fn a_fact_states_no_promotion_it_cannot_reach() {
        let text = text_of_rows(&memory_log_rows(
            &LoggedMemory {
                memory_id: "nod_2",
                text: "the workspace pins rust 1.97.0",
                class: MemoryClass::Fact,
                confidence: 100,
                kind: "domain",
                decays: false,
                promotes_at: 85,
            },
            120,
        ));
        assert!(
            !text.contains("promotes"),
            "a fact named a rung above the top of the ladder: {text}"
        );
        assert!(text.contains("x reject"), "{text}");
    }

    /// **The witness (#5032).** A promotion is **one row**, which SPEC 6.3
    /// states as a property of the event rather than a shape a renderer may
    /// choose: where it moved, on what confidence, and the record that makes
    /// it auditable, all on the line.
    #[test]
    fn a_promotion_renders_as_a_single_row() {
        let rows = memory_promote_rows(
            MemoryClass::Observation,
            MemoryClass::Rule,
            87,
            "prm_dedup_keys_a1b2",
            120,
        );
        assert_eq!(
            rows.len(),
            1,
            "a promotion spilled onto {} rows",
            rows.len()
        );
        let text = text_of_rows(&rows);
        for want in [
            "memory promoted",
            "OBSERVATION → RULE",
            "conf 0.87",
            "audit event prm_dedup_keys_a1b2",
            "now prompt-injected",
        ] {
            assert!(
                text.contains(want),
                "the row does not state `{want}`: {text}"
            );
        }
    }
}
