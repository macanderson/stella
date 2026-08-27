//! **The witness (#5031).** SPEC 6.3's skill event, end to end on the deck.
//!
//! `EventKind::Skill` could render the row for as long as it existed and
//! nothing ever built one: there was no `TranscriptEntry::Skill`, so the only
//! trace of an injected skill on any live surface was a count of what the
//! trust gate had *withheld*. A user could not see which skill fired, how it
//! was triggered, or what it cost.
//!
//! Pinned here: that the fold makes a row out of the wire event at all, that
//! the row carries the three head facts and the injected summary under them,
//! and that the summary is elided rather than faked when a skill has none.

use super::*;
use stella_tui_theme::glyph;

fn injected(name: &str, summary: &str, tokens: u32) -> AgentEvent {
    AgentEvent::SkillInjected {
        name: name.to_string(),
        summary: summary.to_string(),
        tokens,
    }
}

fn rows(event: &AgentEvent) -> String {
    let mut model = SessionModel::new();
    model.apply(event);
    assert_eq!(
        model.transcript.len(),
        1,
        "the fold produced no row: {:?}",
        model.transcript
    );
    transcript_lines(&model, false, 100)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The head reads `✦ skill <name> · auto · n tok`, and the injected summary
/// hangs under it.
#[test]
fn an_injected_skill_renders_the_spec_head_and_its_summary() {
    let text = rows(&injected(
        "oxagen-feature",
        "the 10-layer feature contract",
        1200,
    ));
    assert!(text.contains(glyph::SKILL), "{text}");
    assert!(text.contains("skill"), "{text}");
    assert!(text.contains("oxagen-feature"), "{text}");
    assert!(text.contains("auto"), "the trigger column: {text}");
    assert!(text.contains("1.2k tok"), "the cost column: {text}");
    assert!(
        text.contains("injected the 10-layer feature contract"),
        "{text}"
    );
}

/// SPEC 6.3's `used n× this repo` is **absent**, not zero.
///
/// The store counts it, no live path carries the number here, and a `used 0×`
/// under a skill that has just fired would state the opposite of what
/// happened. #4337 is where the counter's reader is decided; until then the
/// column is elided, and this pins that it stays elided rather than being
/// filled with a placeholder somebody later reads as data.
#[test]
fn the_repo_usage_count_is_elided_rather_than_zeroed() {
    let text = rows(&injected("reviewer", "database review", 40));
    assert!(!text.contains("used"), "{text}");
    assert!(!text.contains('×'), "{text}");
}

/// A skill whose frontmatter carries no description renders the head alone.
///
/// An empty `injected` line would be a body promising a summary and giving
/// none — the shape every optional column in this renderer already refuses
/// (an unmeasured extent draws no column, not `+0 -0`).
#[test]
fn a_skill_with_no_description_renders_no_body_line() {
    let text = rows(&injected("terse", "", 40));
    assert!(text.contains("terse"), "{text}");
    assert!(!text.contains("injected"), "{text}");
}

/// **Golden.** What the block actually looks like at a real pane width — the
/// assertion the `contains` checks above cannot make.
///
/// Pinned for the reason `deck_render_snapshots` exists: a column that
/// shifted, a separator that doubled, or a footer that lost its indent passes
/// every substring assertion in this file and is still wrong on screen. Both
/// rows carry the silver rail SPEC 6.2 gives a skill, and the trailing blank
/// is the block spacer every non-`ToolStart` entry closes on.
#[test]
fn the_rendered_skill_block_is_byte_identical_to_the_pinned_rows() {
    let text = rows(&injected(
        "oxagen-feature",
        "the 10-layer feature contract",
        1200,
    ));
    assert_eq!(
        text,
        concat!(
            " │ ✦ skill oxagen-feature · auto · 1.2k tok\n",
            " │  injected the 10-layer feature contract\n",
        ),
        "the skill block's rows moved:\n{text}"
    );
}
