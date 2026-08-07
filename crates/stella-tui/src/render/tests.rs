// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Coverage for the transcript builders and leaf panels the Command Deck
//! draws with.
//!
//! This module used to also carry a few hundred assertions about a top-level
//! frame composer no product path reached (#936). Those went with the surface
//! they described — a suite that tests an unreachable surface overstates what
//! it protects, which was the whole complaint. What is left is the part the
//! deck genuinely renders through: inline diffs in a tool result, the brand
//! palette every transcript row is drawn from, the slash popup's windowing,
//! and collapsed/expanded reasoning.

use super::*;
use crate::composer::SlashCommand;
use crate::model::SubAgentSummary;
use crate::model::{FileState, SessionModel, TranscriptEntry};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use stella_protocol::{
    AgentEvent, BudgetMode, CiStatus, FileChangeKind, MediaJobState, MediaKind, PrStatus,
    StageKind, SubAgentStatus,
};

mod inline_diff;
mod palette;
mod slash;
mod thinking;

/// Flatten a `Buffer` to one `String` per row (styling stripped — content is
/// what we assert on, never raw ANSI, per L-T6).
fn buffer_rows(buf: &Buffer) -> Vec<String> {
    let area = *buf.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
        })
        .collect()
}

fn buffer_text(buf: &Buffer) -> String {
    buffer_rows(buf).join("\n")
}

/// Fold a whole model's transcript the way a deck lane does — `entry_lines`
/// per entry, then the streaming preview.
///
/// This was a function in `render::entry` until #936: its only caller was the
/// deleted single-session surface, because the deck composes its lanes itself.
/// Keeping it as a *fixture* preserves the assertions below (which are about
/// `entry_lines`, and that is very much live) without keeping a production
/// function nothing calls — the exact trade the issue was about.
fn transcript_lines(
    model: &SessionModel,
    expand_thinking: bool,
    width: usize,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let live = reasoning_is_live(&model.transcript, &model.streaming_text);
    let last = model.transcript.len().saturating_sub(1);
    for (i, entry) in model.transcript.iter().enumerate() {
        entry_lines(
            entry,
            &model.files,
            expand_thinking,
            expand_thinking,
            live && i == last,
            width,
            &mut out,
        );
    }
    streaming_lines(
        &model.streaming_text,
        &model.files,
        expand_thinking,
        width,
        &mut out,
    );
    out
}

/// One entry of every transcript kind, for the palette sweep.
fn sample_entries() -> Vec<TranscriptEntry> {
    vec![
        TranscriptEntry::User("hi".into()),
        TranscriptEntry::Stage(StageKind::Execute),
        TranscriptEntry::Text("ok".into()),
        TranscriptEntry::Reasoning("hmm".into()),
        TranscriptEntry::ToolStart {
            call_id: "c1".into(),
            name: "bash".into(),
            input: "ls".into(),
            raw: "{}".into(),
            path: None,
        },
        TranscriptEntry::ToolResult {
            call_id: "c1".into(),
            name: "bash".into(),
            ok: true,
            summary: "done".into(),
            full: "done".into(),
            duration_ms: 3,
            speculated: false,
            diff: None,
        },
        TranscriptEntry::Retry {
            attempt: 1,
            reason: "rate limit".into(),
        },
        TranscriptEntry::Parked {
            description: "CI for branch main settles".into(),
            poll_interval_secs: 5,
            deadline_secs: 600,
        },
        TranscriptEntry::Woken {
            reason: "changed".into(),
            polls_used: 3,
        },
        TranscriptEntry::Compaction {
            before_tokens: 10,
            after_tokens: 5,
            evicted: 1,
            deduped: 2,
        },
        TranscriptEntry::BudgetTick {
            spent_usd: 0.01,
            limit_usd: Some(1.0),
            mode: BudgetMode::Observed,
        },
        TranscriptEntry::ProviderFallback {
            from: "a".into(),
            to: "b".into(),
            reason: "down".into(),
        },
        TranscriptEntry::ContextRecall {
            frames: 2,
            tokens: 120,
            labels: vec!["adr".into()],
        },
        TranscriptEntry::ContextWrite {
            provider: "mem".into(),
            upserts: 2,
            superseded: 1,
        },
        TranscriptEntry::MediaProgress {
            artifact_id: "m1".into(),
            kind: MediaKind::Image,
            state: MediaJobState::Queued,
        },
        TranscriptEntry::MediaComplete {
            label: "logo".into(),
            path: "out.png".into(),
            kind: MediaKind::Image,
        },
        TranscriptEntry::Verdict {
            passed: true,
            summary: "ok".into(),
            deterministic: true,
        },
        TranscriptEntry::GoalVerdict {
            met: false,
            round: 2,
            reasoning: "tests still red".into(),
        },
        TranscriptEntry::ScopeReview {
            summary: "auth".into(),
            steps: 2,
            estimated_files: 3,
        },
        TranscriptEntry::AskUser {
            question: "which db?".into(),
            options: 2,
        },
        TranscriptEntry::Commit {
            sha: "abc123def456".into(),
            message: "fix".into(),
        },
        TranscriptEntry::Pr {
            url: "https://example.test/pr/1".into(),
            status: PrStatus::Open,
            number: Some(1),
            ci: Some(CiStatus::Passing),
        },
        TranscriptEntry::TaskUpdate {
            done: 2,
            total: 5,
            active: Some("wire the task board".into()),
        },
        TranscriptEntry::Error {
            message: "boom".into(),
            retryable: false,
        },
        TranscriptEntry::Complete {
            model: "glm-5.2".into(),
            cost_usd: 0.1,
        },
        // Both phases: the start and finish rows take different render
        // paths (quiet note vs. status-hued note), so one sample would
        // leave half the arm unexercised by the rail invariant.
        TranscriptEntry::SubAgent {
            agent_id: "search-1".into(),
            finished: None,
            instruction_preview: "find the retry policy".into(),
            write_access: false,
        },
        TranscriptEntry::SubAgent {
            agent_id: "search-1".into(),
            finished: Some(SubAgentSummary {
                status: SubAgentStatus::Completed,
                cost_usd: 0.004,
                steps: 5,
                absorbed_messages: 9,
                reason: None,
            }),
            instruction_preview: String::new(),
            write_access: false,
        },
    ]
}

/// The stat box's content row, flattened.
///
/// `⏳` is double-width, so flattening the buffer leaves its trailing filler
/// cell in the string; the assertions below match on the clock rather than on
/// the glyph's spacing for that reason.
fn hud_row(parked: Option<(&OpenPark, u64)>) -> String {
    let mut buf = Buffer::empty(Rect::new(0, 0, 120, HUD_H));
    render_hud(&Hud::default(), parked, buf.area, &mut buf);
    buffer_rows(&buf).remove(1)
}

/// The rendering half of #2007's witness: the stat box states elapsed against
/// the deadline, and the readout is a function of elapsed alone — so it moves
/// between two frames with no event in between.
///
/// The transcript row this complements states only the *budget* ("up to
/// 1800s") and then sits motionless for the length of the wait, which is why a
/// park ten seconds old and one twenty-nine minutes into its deadline used to
/// look the same, and why a wedged engine looked like both.
#[test]
fn the_hud_counts_an_open_park_up_against_its_deadline() {
    let park = OpenPark {
        description: "CI for branch main settles".into(),
        poll_interval_secs: 30,
        deadline_secs: 1_800,
    };

    let early = hud_row(Some((&park, 10_000)));
    assert!(
        early.contains("parked 0:10 / 30:00"),
        "elapsed over the deadline, both in one unit: {early}"
    );
    assert!(
        early.contains('⏳'),
        "chipped like the transcript row: {early}"
    );
    assert!(
        early.contains("CI for branch main settles"),
        "and which wait it is: {early}"
    );

    let late = hud_row(Some((&park, 1_752_000)));
    assert!(late.contains("parked 29:12 / 30:00"), "{late}");
    assert_ne!(
        early, late,
        "the same park at two moments must not render identically — that is \
         the whole defect"
    );
}

/// An hour-long wait rolls to `H:MM:SS` rather than printing `73:20`, and a
/// turn that is not parked pays nothing at all — which is also why the deck's
/// golden frames are undisturbed by this feature.
#[test]
fn the_park_clock_rolls_past_an_hour_and_is_absent_when_not_parked() {
    let long = OpenPark {
        description: "the nightly suite finishes".into(),
        poll_interval_secs: 60,
        deadline_secs: 7_200,
    };
    let row = hud_row(Some((&long, 4_400_000)));
    assert!(row.contains("parked 1:13:20 / 2:00:00"), "{row}");

    let idle = hud_row(None);
    assert!(
        !idle.contains("parked"),
        "no chip when nothing is parked: {idle}"
    );
}

/// A tool is free to write a paragraph into `TurnParked.description`, and the
/// stat box is one line. The subject is capped so the clock — the part that
/// cannot be read anywhere else — is never the thing pushed off the row.
#[test]
fn a_long_park_description_cannot_push_the_clock_off_the_box() {
    let wordy = OpenPark {
        description: "the continuous integration pipeline for the release \
                      branch reaches a terminal state"
            .into(),
        poll_interval_secs: 30,
        deadline_secs: 600,
    };
    let row = hud_row(Some((&wordy, 65_000)));
    assert!(row.contains("parked 1:05 / 10:00"), "{row}");
    assert!(
        row.contains('…'),
        "the subject is elided, not the clock: {row}"
    );
}
