// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Stella's own voice is distinguishable from the model's — SPEC 12.4's
//! `silver rail`.
//!
//! #5056 put the panel handshake in the transcript; it arrived as a fabricated
//! `AgentEvent::Text`, which renders on the **agent** rail. So a consent
//! document — the most security-relevant thing the transcript ever carries —
//! was visually indistinguishable from the model talking, which is the wrong
//! direction for SPEC 12.3's whole argument (#5300).
//!
//! The `▸` marker was already on the text and is the half that survives being
//! read aloud. What was missing is the visual half, and these pin that the two
//! agree about who spoke.

use super::*;
use crate::accessible::NOTICE_MARKER;
use crate::render::row::Rail;
use stella_tui_theme::token;

const WIDTH: usize = 80;

fn rows(entry: &TranscriptEntry) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    entry_lines(
        entry,
        EntryView::default(),
        false,
        false,
        false,
        WIDTH,
        &mut out,
    );
    out
}

/// The first span of the first row — the rail cell, before any content.
fn rail_cell(lines: &[Line<'static>]) -> (String, Option<ratatui::style::Color>) {
    let span = lines
        .first()
        .and_then(|line| line.spans.first())
        .expect("a rendered row opens with its rail");
    (span.content.to_string(), span.style.fg)
}

/// **The witness (#5300).** Host prose rails silver; the model's does not rail
/// at all.
///
/// Asserted as a *contrast* rather than against a pinned column, because the
/// requirement is that the two are distinguishable — a test that pinned only
/// the host row would still pass if agent prose grew the same rail.
#[test]
fn stellas_own_voice_rails_silver_and_the_models_does_not() {
    let host = rows(&TranscriptEntry::Text(format!(
        "{NOTICE_MARKER}stella-candidates asks to draw a panel. It may show text \
         and read this session's plan. a accept · d decline"
    )));
    let (cell, metal) = rail_cell(&host);
    assert_eq!(
        metal,
        Some(token::SILVER),
        "a consent document must not wear the model's rail: {cell:?}"
    );
    assert_eq!(cell, Rail::Host.prefix());

    let agent = rows(&TranscriptEntry::Text(
        "I have finished the refactor.".to_string(),
    ));
    let (agent_cell, agent_metal) = rail_cell(&agent);
    assert_ne!(
        agent_cell, cell,
        "the two voices must differ in the margin, not only in colour — \
         SPEC 13 requires the distinction to survive with colour off"
    );
    assert_ne!(agent_metal, Some(token::SILVER));
}

/// The marker stays on the text, and is not replaced by the rail.
///
/// `chrome_note`'s doc comment carries the argument: a rail glyph is a
/// *visual* distinction, which is exactly the one that does not survive being
/// read aloud. The colour is the addition, never the substitution.
#[test]
fn the_rail_is_added_to_the_marker_and_does_not_replace_it() {
    let host = rows(&TranscriptEntry::Text(format!(
        "{NOTICE_MARKER}conversation cleared"
    )));
    let text: String = host
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect();
    assert!(
        text.contains(NOTICE_MARKER.trim()),
        "the accessible half must survive the visual one: {text:?}"
    );
}
