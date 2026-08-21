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
        entry_lines(
            &entry,
            EntryView::default(),
            true,
            true,
            false,
            80,
            &mut lines,
        );
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
        EntryView::default(),
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
        entry_lines(
            entry,
            EntryView::default(),
            false,
            false,
            false,
            80,
            &mut out,
        );
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
        Some(stella_tui_theme::token::MUTED),
        "a read is the world coming in, not stella acting, so SPEC 6.2 gives \
         it the silver-dim rail and reserves gold for the calls that change \
         something",
    );
    assert_eq!(
        prefix_fg(&TranscriptEntry::ToolStart {
            call_id: "c2".into(),
            name: "edit_file".into(),
            input: "x".into(),
            raw: "{}".into(),
            path: Some("src/lib.rs".into()),
        }),
        Some(stella_tui_theme::token::GOLD),
        "a mutation is stella acting — the gold half of the two-metal rule",
    );
    assert_eq!(
        prefix_fg(&TranscriptEntry::ToolResult {
            call_id: "c1".into(),
            name: "read_file".into(),
            path: None,
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
            path: None,
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
    // first column is the rule's own seam rather than a status glyph. This is
    // the one entry whose left edge is deliberately structural: a phase
    // boundary should recede until you scroll for it, and four bolded
    // `◇ stage` rows in twenty lines is what it looked like when it did not.
    //
    // The *louder* seam, though. The plain hairline is 1.26:1 and is
    // documented as never being the only carrier of structure — but a stage
    // rule is precisely the transcript's coarsest structure, and at that
    // contrast neither the line nor the label set into it could be read.
    assert_eq!(
        prefix_fg(&TranscriptEntry::Stage {
            name: StageKind::Execute.into(),
            opens: None,
        }),
        Some(theme::HAIRLINE_STRONG),
        "a stage opens a section rule on the louder seam",
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

/// **The witness for the transcript's tool-class colours.**
///
/// Before this, every tool name in the scrollback rendered in one colour —
/// the brand gold — so the row said *a tool ran* and nothing more. The class
/// is what a reader actually wants from a log of actions, and it is now
/// carried by the one span they scan for.
///
/// Asserted from both ends: the name wears its class hue, and two calls of
/// different classes do not agree. A regression that put every name back on
/// one colour fails the second half even if someone re-pointed the first.
#[test]
fn a_tool_name_is_hued_by_what_kind_of_call_it_was() {
    let name_fg = |tool: &str| -> Option<Color> {
        let mut out = Vec::new();
        entry_lines(
            &TranscriptEntry::ToolStart {
                call_id: "c".into(),
                name: tool.into(),
                input: "x".into(),
                raw: "{}".into(),
                path: None,
            },
            EntryView::default(),
            false,
            false,
            false,
            80,
            &mut out,
        );
        // SPEC 6.2 head order: 0 is the rail, 1 the kind glyph, 2 the object of
        // the verb — the tool name, for a tool v2 has no verb for.
        out[0].spans[2].style.fg
    };
    for tool in [
        "get_state",
        "save_state",
        "mcp__github__create_pull_request",
        "delegate",
    ] {
        assert_ne!(
            name_fg(tool),
            Some(theme::ACCENT),
            "`{tool}` must not wear the reserved gold — every name doing so is \
             what made the accent mean nothing"
        );
        assert_eq!(
            name_fg(tool),
            Some(stella_tui_theme::token::TEXT),
            "`{tool}` is the object of the verb and takes the text tone; the \
             rail and glyph beside it are what carry the metal (SPEC 6.2)"
        );
    }
    // What replaced the class hue: the *kind* of action, carried by the rail
    // and the glyph rather than by the name. A read is not an edit is not a
    // delete, and that distinction survives a 16-colour terminal because the
    // glyph carries it too (SPEC 13).
    let rail_fg = |tool: &str| -> Option<Color> {
        let mut out = Vec::new();
        entry_lines(
            &TranscriptEntry::ToolStart {
                call_id: "c".into(),
                name: tool.into(),
                input: "x".into(),
                raw: "{}".into(),
                path: Some("src/lib.rs".into()),
            },
            EntryView::default(),
            false,
            false,
            false,
            80,
            &mut out,
        );
        out[0].spans[0].style.fg
    };
    for (a, b) in [
        ("read_file", "edit_file"),
        ("edit_file", "delete_file"),
        ("read_file", "delete_file"),
    ] {
        assert_ne!(rail_fg(a), rail_fg(b), "`{a}` and `{b}` must differ");
    }
}

/// An argument-less call renders no argument column.
///
/// `get_environment` takes no arguments; the compact-JSON fallback printed a
/// literal `{}` beside it, and a reader — the deck's own owner — read that as
/// the tool having returned an empty object. There is no honest rendering of
/// "nothing"; absence is the rendering.
#[test]
fn an_argument_less_call_prints_no_empty_object() {
    let mut out = Vec::new();
    entry_lines(
        &TranscriptEntry::ToolStart {
            call_id: "c".into(),
            name: "get_environment".into(),
            input: String::new(),
            raw: "{}".into(),
            path: None,
        },
        EntryView::default(),
        false,
        false,
        false,
        80,
        &mut out,
    );
    let rendered: String = out[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        rendered.contains("get_environment"),
        "the call is still named: {rendered:?}"
    );
    assert!(
        !rendered.contains("{}"),
        "an empty argument object must not reach the row: {rendered:?}"
    );
}

/// A sub-agent dispatch is the most consequential row a transcript can carry —
/// another agent starts here, with its own budget and its own tool calls — and
/// it rendered in the same quiet tier as the compaction and spend bookkeeping.
#[test]
fn a_sub_agent_dispatch_is_not_bookkeeping() {
    let mut out = Vec::new();
    entry_lines(
        &TranscriptEntry::SubAgent {
            agent_id: "sub:1".into(),
            finished: None,
            instruction_preview: "read the layout".into(),
            write_access: false,
        },
        EntryView::default(),
        false,
        false,
        false,
        80,
        &mut out,
    );
    let label = out[0].spans[0].style;
    assert_eq!(
        label.fg,
        Some(crate::tool_class::ToolClass::Delegate.color()),
        "a dispatch wears the delegation class, like the `delegate` call that made it"
    );
    assert!(
        label.add_modifier.contains(Modifier::BOLD),
        "and it is bold, like every row a scan should stop on"
    );
    for tier in [theme::TEXT_TERTIARY, theme::TEXT_SECONDARY] {
        assert_ne!(label.fg, Some(tier), "a dispatch is not a neutral note");
    }
}
