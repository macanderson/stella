//! Where a transcript row's colour comes from: the palette, and only the
//! palette.
//!
//! Split out of `render/tests.rs` when that file crossed the 1500-line guard,
//! following `thinking`. One topic per file: these three approach one contract
//! from three sides — no row carries a raw `ratatui` named colour, the user's
//! own prompt stays a single violet end to end, and every rail prefix stays
//! inside the brand family.

use super::*;

/// Every colour a transcript row emits must come from the palette.
///
/// The palette exists so a recolour is one edit, and a `Color::Cyan` written
/// inline is invisible to it — it survives every recolour looking deliberate.
/// That is not hypothetical: the SESSION tab shipped with 312 cells of literal
/// ANSI cyan in its HUD and scope card, untouched by two palette generations,
/// because nothing asserted this. `ratatui`'s named variants are the whole
/// failure mode, so the check is simply that no span carries one.
///
/// `Reset` is allowed: it is the deliberate "no colour" for the glyph-less
/// prose rail and for spacer runs, and under `NO_COLOR` every cell becomes it.
#[test]
fn no_transcript_row_carries_a_raw_ansi_colour() {
    for entry in sample_entries() {
        let mut lines = Vec::new();
        entry_lines(&entry, &[], true, true, false, 80, &mut lines);
        for line in &lines {
            for span in &line.spans {
                for (slot, colour) in [("fg", span.style.fg), ("bg", span.style.bg)] {
                    let Some(colour) = colour else { continue };
                    assert!(
                        matches!(colour, Color::Rgb(..) | Color::Reset),
                        "{entry:?} renders {slot} {colour:?} — a named ANSI colour, \
                         which no recolour can reach. Use a `theme::` role.",
                    );
                }
            }
        }
    }
}

// ---- Transcript prefix colors (gold gutter + violet user prompt) ----

/// The user prompt is the single exception to the gold gutter: its
/// `[user]:` tag AND every line of the prompt render in exactly the violet
/// used for the composer's keybind glyphs and the deterministic-first
/// chip — as one flat color, with no markdown tinting and no brand gold.
#[test]
fn user_prompt_entry_is_one_violet_color_end_to_end() {
    let mut out = Vec::new();
    // Markdown that WOULD tint under `markdown::render` (a code span goes
    // `theme::WARN`, a heading goes bold `INK`): proof none of it leaks.
    entry_lines(
        &TranscriptEntry::User("fix the `parser` bug\nand **ship** it".to_string()),
        &[],
        false,
        false,
        false,
        80,
        &mut out,
    );
    let mut saw_text = false;
    for line in &out {
        for span in &line.spans {
            // Skip pure-whitespace gutter/continuation indent (no fg).
            if span.content.trim().is_empty() {
                continue;
            }
            saw_text = true;
            assert_eq!(
                span.style.fg,
                Some(theme::VIOLET),
                "user entry span {:?} is not the violet accent",
                span.content
            );
            // Belt and suspenders: no status/brand hue anywhere on the entry.
            for banned in [theme::ACCENT, theme::DANGER, theme::WARN] {
                assert_ne!(
                    span.style.fg,
                    Some(banned),
                    "user entry leaks a status/brand color: {:?}",
                    span.content
                );
            }
        }
    }
    assert!(
        saw_text,
        "the prompt rendered at least one styled text span"
    );
}

/// Every rail and note glyph stays inside the warm brand family — failure
/// crimson, calls deep gold, prompts violet — so no raw ANSI cyan/blue/
/// magenta survives from before the palette landed.
///
/// The margin's colour is itself the hierarchy, in three steps: the `●`
/// call rail is brand gold because the call *is* the event; the `⎿` result
/// rail is muted because a successful outcome is that event's consequence,
/// subordinate chrome rather than a second thing to look at; and the `✗`
/// fail rail is danger, the one outcome worth interrupting a scan for.
/// Painting every ✓ as loudly as every ✗ would leave a failure with nothing
/// to stand out against.
#[test]
fn transcript_prefix_colors_stay_in_the_brand_family() {
    let prefix_fg = |entry: &TranscriptEntry| -> Option<Color> {
        let mut out = Vec::new();
        entry_lines(entry, &[], false, false, false, 80, &mut out);
        out[0].spans[0].style.fg
    };
    assert_eq!(
        prefix_fg(&TranscriptEntry::Error {
            message: "boom".into(),
            retryable: false,
        }),
        Some(theme::DANGER),
        "error prefix is crimson",
    );
    assert_eq!(
        prefix_fg(&TranscriptEntry::ToolStart {
            call_id: "c1".into(),
            name: "read_file".into(),
            input: "x".into(),
            raw: "{}".into(),
            path: None,
        }),
        Some(theme::ACCENT_DEEP),
        "a tool call — the thing that happened — carries the deep-gold rail",
    );
    assert_eq!(
        prefix_fg(&TranscriptEntry::ToolResult {
            call_id: "c1".into(),
            name: "read_file".into(),
            ok: true,
            summary: "ok".into(),
            full: "ok".into(),
            duration_ms: 3,
            speculated: false,
            diff: None,
        }),
        Some(theme::MUTED),
        "a successful result's rail recedes — it is a continuation of its \
         call, not a second event competing for the eye",
    );
    assert_eq!(
        prefix_fg(&TranscriptEntry::ToolResult {
            call_id: "c2".into(),
            name: "read_file".into(),
            ok: false,
            summary: "no".into(),
            full: "no".into(),
            duration_ms: 3,
            speculated: false,
            diff: None,
        }),
        Some(theme::DANGER),
        "failed tool-result prefix is crimson",
    );
    // A stage is a section rule, not a row (see `row::push_rule`), so its
    // first column is the rule's own hairline rather than a status glyph. This
    // is the one entry whose left edge is deliberately structural: a phase
    // boundary should recede until you scroll for it, and four bolded `◇ stage`
    // rows in twenty lines is what it looked like when it did not.
    assert_eq!(
        prefix_fg(&TranscriptEntry::Stage(StageKind::Execute)),
        Some(theme::HAIRLINE),
        "a stage opens a hairline section rule",
    );
    // A prompt is the strongest landmark in the scrollback — where a reader
    // re-orients when scrolling back — and rides the interactive-chrome
    // violet shared with the composer, never the reserved brand accent.
    assert_eq!(
        prefix_fg(&TranscriptEntry::User("hi".into())),
        Some(theme::VIOLET),
        "the user rail is violet",
    );
    // Assistant prose is the one rail with no colour, because it has no
    // glyph to colour: `Rail::Agent` is two bare spaces. Prose is the
    // transcript's default voice, and a marker on every paragraph would be
    // noise exactly where a reader is reading rather than scanning.
    assert_eq!(
        prefix_fg(&TranscriptEntry::Text("ok".into())),
        None,
        "agent prose rides an unstyled, glyph-less rail",
    );
}
