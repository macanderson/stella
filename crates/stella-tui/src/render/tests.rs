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

mod block_rail;
mod inline_diff;
mod palette;
mod slash;
mod thinking;
mod tool_output;

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
            EntryView::at(&model.files, &model.transcript, i),
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
        TranscriptEntry::Stage(StageKind::Execute.into()),
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
            path: None,
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
            frames: vec![crate::model::RecalledFrameRow {
                kind: "memory".into(),
                label: "adr".into(),
                uri: None,
                provider: "workspace-memory".into(),
                source: "stella-context".into(),
                method: None,
                id: None,
                digest: None,
                tokens: 120,
            }],
            tokens: 120,
            latency_ms: 12,
            used_ann_index: Some(true),
            providers: vec![("workspace-memory".into(), 1)],
            budget: None,
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
            turn: 1,
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

// ── Context recall ──────────────────────────────────────────────────────────

/// The recall from the report that prompted this work: four code-graph symbols
/// and one episodic memory, 1155 tokens between them.
fn screenshot_recall() -> TranscriptEntry {
    fn symbol(label: &str, uri: &str, tokens: u32) -> crate::model::RecalledFrameRow {
        crate::model::RecalledFrameRow {
            kind: "symbol".into(),
            label: label.into(),
            uri: Some(uri.into()),
            provider: "code-graph".into(),
            source: "stella-graph".into(),
            method: Some("symbol-name".into()),
            id: None,
            digest: Some("sha256:9f2c1abfeed".into()),
            tokens,
        }
    }
    TranscriptEntry::ContextRecall {
        frames: vec![
            symbol("fn line", "arenabench/recorder/render.py:294", 82),
            symbol("table runs", "bench/telemetry_store/schema.sql:22", 61),
            symbol(
                "fn review",
                "crates/stella-cli/src/command_deck/hunk_gate.rs:32",
                104,
            ),
            symbol(
                "fn review",
                "crates/stella-cli/src/command_deck/scope_gate.rs:90",
                96,
            ),
            crate::model::RecalledFrameRow {
                kind: "episode".into(),
                label: "create a new minor release 0.8.0 for stella and build a \
                        release notes page to publish"
                    .into(),
                uri: None,
                provider: "workspace-memory".into(),
                source: "stella-context".into(),
                method: Some("embedding".into()),
                id: Some("nod_01HQZ".into()),
                digest: None,
                tokens: 812,
            },
        ],
        tokens: 1155,
        latency_ms: 34,
        used_ann_index: Some(true),
        providers: vec![("code-graph".into(), 4), ("workspace-memory".into(), 1)],
        budget: Some(crate::model::RecallBudget {
            requested: 4000,
            consumed: 1155,
            providers: vec![
                ("code-graph".into(), 4, 0, 343),
                ("workspace-memory".into(), 1, 2, 812),
            ],
        }),
    }
}

/// SPEC 6.1: a turn ends on a labelled rule and one receipt line.
///
/// The witness for the boundary landing. On the old renderer a completed turn
/// was a single `✓ cost` note, so every assertion here fails on it: there was
/// no rule, no `receipt` line, and no turn number anywhere in the transcript.
#[test]
fn a_completed_turn_closes_on_a_rule_and_a_receipt() {
    let entry = TranscriptEntry::Complete {
        model: "glm-5.2".into(),
        cost_usd: 0.11,
        turn: 14,
    };
    let text = recall_text(&entry, false, 100);

    assert!(text.contains("turn 14 done"), "no closing rule:\n{text}");
    assert!(
        text.contains("──"),
        "the rule does not reach the row:\n{text}"
    );
    assert!(text.contains("receipt"), "no receipt line:\n{text}");
    assert!(
        text.contains("$0.11"),
        "the receipt lost the spend:\n{text}"
    );
    assert!(
        text.contains("↵ audit"),
        "the receipt lost its affordance:\n{text}"
    );

    // Nothing the deck cannot measure. `StepUsage` is deliberately unfolded,
    // there is no per-turn clock, and no per-turn file/test/memory count — so a
    // receipt claiming any of them would be reporting a measurement nobody
    // took. They elide until something counts them.
    for absent in ["tok", "det ", "tests", "file", "memory", "0:00"] {
        assert!(
            !text.contains(absent),
            "the receipt invented {absent:?}:\n{text}"
        );
    }
}

/// SPEC 2: money is gold. The v1 row rendered a settled turn cost in
/// `SUCCESS_BRIGHT` green, which spends the pass colour on an amount — and
/// green is reserved for pass semantics, not for spending.
#[test]
fn the_turn_receipt_prices_in_gold_not_in_the_pass_colour() {
    let mut out = Vec::new();
    entry_lines(
        &TranscriptEntry::Complete {
            model: "glm-5.2".into(),
            cost_usd: 0.11,
            turn: 14,
        },
        EntryView::default(),
        false,
        false,
        false,
        100,
        &mut out,
    );
    let money = out
        .iter()
        .flat_map(|l| l.spans.iter())
        .find(|s| s.content.contains("$0.11"))
        .expect("the receipt renders the spend");
    assert_eq!(
        money.style.fg,
        Some(stella_tui_theme::token::GOLD),
        "money is gold (SPEC 2/5)"
    );
    for span in out.iter().flat_map(|l| l.spans.iter()) {
        assert_ne!(
            span.style.fg,
            Some(stella_tui_theme::token::GREEN),
            "green is pass semantics; a turn ending is not a pass: {:?}",
            span.content
        );
    }
}

fn recall_text(entry: &TranscriptEntry, expanded: bool, width: usize) -> String {
    let mut out = Vec::new();
    entry_lines(
        entry,
        EntryView::default(),
        false,
        expanded,
        false,
        width,
        &mut out,
    );
    out.iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
#[ignore = "eyeball the layout: cargo test -p stella-tui show_recall -- --ignored --nocapture"]
fn show_recall() {
    println!(
        "\n── collapsed ──\n{}\n\n── ctrl+o ──\n{}\n",
        recall_text(&screenshot_recall(), false, 100),
        recall_text(&screenshot_recall(), true, 100)
    );
}

/// A recall renders as a table — one row per frame — not as its labels joined
/// into a paragraph.
///
/// The paragraph is what this replaces, and it failed in four separate ways at
/// once: no boundary between records, no way to tell an 812-token episodic
/// memory from a 61-token graph symbol, no per-frame cost at all, and a final
/// label truncated mid-word by the pane edge. Each assertion below pins one of
/// those, so a regression names which property came back.
#[test]
fn a_recall_renders_as_a_table_not_a_paragraph() {
    let text = recall_text(&screenshot_recall(), false, 100);

    // The header carries the two totals plus the two facts that say whether
    // recall was the reason the turn felt slow.
    assert!(text.contains("5 frames · 1155 tok"), "{text}");
    assert!(text.contains("34ms"), "{text}");
    assert!(text.contains("ann"), "{text}");

    // One row per frame, each naming its kind — the field that separates a
    // graph symbol from a recalled prompt, and the field the old rendering
    // dropped entirely.
    assert!(text.contains("symbol"), "{text}");

    // Per-frame cost, which is what turns "1155 tok" from a number into a
    // finding: one frame is holding 70% of the turn's context budget.
    assert!(text.contains("104 tok"), "{text}");

    // The frames are on their own rows, never comma-joined into prose.
    assert!(
        !text.contains("fn line, table runs"),
        "labels must not be run together: {text}"
    );
    for line in text.lines() {
        assert!(
            line.matches(" tok").count() <= 1,
            "one frame per row: {line:?}"
        );
    }
}

/// The collapsed row bounds itself, and says so.
///
/// The old paragraph grew with the recall — five frames wrapped to four rows,
/// and a ten-frame recall would have buried the turn under its own context
/// report before the turn produced anything.
#[test]
fn a_collapsed_recall_is_bounded_and_offers_the_rest() {
    let text = recall_text(&screenshot_recall(), false, 100);
    assert!(
        text.lines().count() <= 6,
        "collapsed recall must stay bounded, got {} rows:\n{text}",
        text.lines().count()
    );
    assert!(text.contains("2 more"), "{text}");
    assert!(text.contains("ctrl+o"), "{text}");
    // The folded frames are genuinely absent, not merely elided — a fold that
    // still pays for its content is not a fold.
    assert!(
        !text.contains("release notes page"),
        "the folded frames must not still be rendered: {text}"
    );
    // …but the fold names what it is hiding, in the unit that matters. The two
    // folded frames here carry 908 of the turn's 1155 context tokens, so a
    // reader learns there is an outlier behind the fold without the renderer
    // having to reorder the frames away from what the model actually saw.
    assert!(
        text.contains("908 tok"),
        "the fold must state the cost it hides: {text}"
    );
}

/// `ctrl+o` reveals every frame plus the provenance and budget that have no
/// other surface at all.
///
/// This is the affordance that did nothing before: `is_expandable` did not list
/// the variant, and the render arm ignored the flag it was handed, so the row
/// with the most behind it was the one row `ctrl+o` skipped.
#[test]
fn ctrl_o_reveals_provenance_and_the_budget_report() {
    let collapsed = recall_text(&screenshot_recall(), false, 100);
    let expanded = recall_text(&screenshot_recall(), true, 100);

    assert!(
        expanded.lines().count() > collapsed.lines().count(),
        "expanding must reveal something:\ncollapsed:\n{collapsed}\nexpanded:\n{expanded}"
    );

    // Every frame, including the two the fold held back.
    assert!(expanded.contains("table runs"), "{expanded}");
    assert!(expanded.contains("812 tok"), "{expanded}");

    // The provenance chain: the adapter, the store behind it, the method.
    // `provider ← source` is two fields on purpose — `workspace-memory`
    // fronting `stella-context` is exactly the case one field would hide.
    assert!(expanded.contains("code-graph ← stella-graph"), "{expanded}");
    assert!(expanded.contains("embedding"), "{expanded}");

    // A frame with no digest is not verifiable per the context-reuse spec, and
    // the absence is reported rather than rendered as an empty field.
    assert!(expanded.contains("unverifiable"), "{expanded}");

    // The budget report, whose `rejected` count the frame list cannot carry:
    // a rejected frame never reaches it.
    assert!(expanded.contains("budget 1155 of 4000 tok"), "{expanded}");
    assert!(expanded.contains("2 rejected"), "{expanded}");
}

/// The collapsed row cites by human label; the raw id appears only once
/// `ctrl+o` has been pressed.
///
/// That split *is* L-C4 — the id "belongs only in inspectable detail views,
/// never as the primary identifier" — and it is a renderer property, since the
/// read-model must carry the id for the detail view to have anything to show.
#[test]
fn collapsed_recall_cites_by_label_never_id() {
    let entry = TranscriptEntry::ContextRecall {
        frames: vec![crate::model::RecalledFrameRow {
            kind: "memory".into(),
            label: "prefer rg over grep".into(),
            uri: None,
            provider: "workspace-memory".into(),
            source: "stella-context".into(),
            method: None,
            id: Some("nod_913d6df1".into()),
            digest: None,
            tokens: 40,
        }],
        tokens: 40,
        latency_ms: 5,
        used_ann_index: None,
        providers: vec![("workspace-memory".into(), 1)],
        budget: None,
    };
    let collapsed = recall_text(&entry, false, 100);
    assert!(collapsed.contains("prefer rg over grep"), "{collapsed}");
    assert!(
        !collapsed.contains("nod_913d6df1"),
        "the raw id must not reach the collapsed row: {collapsed}"
    );
    assert!(
        recall_text(&entry, true, 100).contains("nod_913d6df1"),
        "the detail view is where the id belongs"
    );
}

/// A location is elided from the *left*, keeping the filename and line.
///
/// The pane edge did the opposite: it clipped from the right, which removed
/// exactly the discriminating tail (`hunk_gate.rs:32`) and kept the repo prefix
/// every row on screen already shares.
#[test]
fn a_long_location_keeps_its_filename_and_line() {
    let text = recall_text(&screenshot_recall(), true, 100);
    assert!(text.contains("hunk_gate.rs:32"), "{text}");
    assert!(text.contains("scope_gate.rs:90"), "{text}");
}

/// The `path:line` survives at every pane width that keeps a location column
/// at all, because the *label* absorbs the pressure.
///
/// This is the failure the deck's pty smoke test caught, and it is the reason
/// the block computes its own columns instead of handing the whole left side to
/// `justify`: `justify` truncates its left column from the right, so at 100
/// columns it rendered `crates/stella-core/src/driver.r…` — the repo prefix
/// every row already shares, with the filename and line deleted. The two
/// columns fail in opposite directions and only one of them elides gracefully.
#[test]
fn narrowing_the_pane_eats_the_label_not_the_line_number() {
    for width in [70, 80, 100, 120, 200] {
        let text = recall_text(&screenshot_recall(), false, width);
        assert!(
            text.contains("hunk_gate.rs:32"),
            "the filename and line must survive at width {width}:\n{text}"
        );
    }
}

/// Below the width where a location could still say anything, the column is
/// dropped whole rather than rendered as an ellipsis with a suffix.
///
/// Two starved columns are worse than one good one: `…e.rs:32` beside `fn r…`
/// costs the same rows and answers neither question.
#[test]
fn a_pane_too_narrow_for_a_location_drops_the_column_not_the_row() {
    let text = recall_text(&screenshot_recall(), false, 40);
    assert!(text.contains("symbol"), "the rows still render: {text}");
    assert!(text.contains("tok"), "the cost still renders: {text}");
    assert!(
        !text.contains("hunk_gate"),
        "a location that cannot fit is dropped, not stubbed: {text}"
    );
}

/// Every frame row in a block lands its token count in the same column.
///
/// Alignment is the whole reason this is a table and not a paragraph — a
/// ragged metric edge is just a list again, and the outlier that made the case
/// for per-frame costs is only visible because the digits stack.
#[test]
fn the_token_column_is_aligned_across_a_block() {
    let text = recall_text(&screenshot_recall(), true, 100);
    // Frame rows only — the budget breakdown below them ends in `tok` too, and
    // it is a different table with its own columns.
    //
    // Measured in **display columns**, which is the unit a terminal lays out
    // in and the unit the table is fitted in. `str::len` reports a phantom
    // two-column drift on every row an elision touched (`…` is one column and
    // three bytes), and `chars().count()` — which this asserted in before —
    // agrees with the truth only while every character is one column wide, so
    // it cannot see the failure `a_wide_citation_never_shifts_its_neighbours`
    // is about.
    let ends: Vec<usize> = text
        .lines()
        .filter(|l| {
            l.ends_with(" tok")
                && matches!(l.trim_start().split(' ').next(), Some("symbol" | "episode"))
        })
        .map(columns)
        .collect();
    assert!(ends.len() >= 4, "expected frame rows, got {ends:?}");
    assert!(
        ends.iter().all(|e| *e == ends[0]),
        "token counts must share a column, got line ends {ends:?}:\n{text}"
    );
}

/// Display columns a rendered row occupies — the unit the table is fitted in.
fn columns(line: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(line)
}

/// A citation label of double-width characters does not move the columns to
/// its right.
///
/// This is the failure a `char`-budgeted table cannot see and an ASCII fixture
/// cannot provoke. The elision budget, the location's left-elision and the
/// per-block column fit were all counted in `char`s while the padding that
/// realises them was counted in display columns, so a 33-character CJK label
/// passed a 34-*column* budget, rendered 66 columns wide, and shoved the
/// location and the cost off the grid on that row alone. Recalled labels are
/// model- and user-authored prose — a recalled Chinese commit message or an
/// emoji in an episodic memory's title is an input, not a hypothetical.
#[test]
fn a_wide_citation_never_shifts_its_neighbours() {
    let mut entry = screenshot_recall();
    let TranscriptEntry::ContextRecall { frames, .. } = &mut entry else {
        unreachable!()
    };
    // 40 double-width characters: 40 `char`s, 80 columns.
    frames[0].label =
        "在上下文中回忆一个符号并计算其令牌成本以便对齐列宽和缩进的一个很长的标题".into();
    frames[1].label = "短标题".into();

    let text = recall_text(&entry, true, 100);
    let rows: Vec<&str> = text
        .lines()
        .filter(|l| l.trim_start().starts_with("symbol "))
        .collect();
    assert_eq!(rows.len(), 4, "expected the four symbol rows:\n{text}");

    let widths: Vec<usize> = rows.iter().copied().map(columns).collect();
    assert!(
        widths.iter().all(|w| *w == widths[0]),
        "a wide label must not change a row's width — got {widths:?}:\n{text}"
    );
    // The location column is what a shifted label displaces first, and the
    // filename is what a reader came to the row for.
    let starts: Vec<Option<usize>> = rows
        .iter()
        .map(|l| {
            l.find(".py:")
                .or_else(|| l.find(".sql:"))
                .or_else(|| l.find(".rs:"))
        })
        .collect();
    assert!(
        starts.iter().all(Option::is_some),
        "every location survives a wide neighbour: {starts:?}\n{text}"
    );
}

/// Under `ctrl+o` the table names its columns, and the rule under each heading
/// is exactly that column wide.
///
/// A heading that does not sit over its own cells is worse than none — it
/// asserts a grid the rows do not keep — so the rule is drawn per column
/// rather than as one hairline, which makes a drifted column visible in the
/// chrome itself.
#[test]
fn the_expanded_table_names_its_columns() {
    let text = recall_text(&screenshot_recall(), true, 100);
    let head = text
        .lines()
        .find(|l| l.trim_start().starts_with("kind "))
        .unwrap_or_else(|| panic!("no column heading:\n{text}"));
    let rule = text
        .lines()
        .find(|l| l.trim_start().starts_with('─'))
        .unwrap_or_else(|| panic!("no column rule:\n{text}"));

    for column in ["kind", "citation", "location", "cost"] {
        assert!(head.contains(column), "heading names {column}:\n{text}");
    }
    // Same width to the column, and the same right edge as the rows they head.
    assert_eq!(columns(head), columns(rule), "heading vs rule:\n{text}");
    let row = text
        .lines()
        .find(|l| l.trim_start().starts_with("symbol "))
        .unwrap_or_else(|| panic!("no frame row:\n{text}"));
    assert_eq!(columns(head), columns(row), "heading vs frame row:\n{text}");

    // Each rule segment is one column's width: `kind` is fitted to the kinds
    // present (`episode`, seven columns), never left at a constant.
    let segments: Vec<usize> = rule
        .trim()
        .split(' ')
        .filter(|s| !s.is_empty())
        .map(columns)
        .collect();
    assert_eq!(
        segments.first().copied(),
        Some(7),
        "the kind rule tracks the widest kind in the block: {segments:?}\n{text}"
    );

    // Collapsed the chrome is absent: the block is held to the height of the
    // paragraph it replaced, and two rows of headings would spend that budget
    // on labels rather than on frames.
    let collapsed = recall_text(&screenshot_recall(), false, 100);
    assert!(
        !collapsed.contains("citation"),
        "the heading is a ctrl+o affordance:\n{collapsed}"
    );
}

/// The budget legs are a grid too — a `·`-joined line put two costs in
/// different places on the one pair of rows where comparing them is the point.
///
/// `4 served · 343 tok` above `1 served · 2 rejected · 812 tok` is the shape
/// this replaces: the rejected count pushes the cost eleven columns right, so
/// the two numbers a reader is there to compare never share an edge.
#[test]
fn the_budget_legs_share_their_columns() {
    let text = recall_text(&screenshot_recall(), true, 100);
    let legs: Vec<&str> = text.lines().filter(|l| l.contains(" served")).collect();
    assert_eq!(legs.len(), 2, "expected both legs:\n{text}");

    let costs: Vec<usize> = legs
        .iter()
        .map(|l| columns(&l[..l.rfind(" tok").expect("a leg reports its cost")]))
        .collect();
    assert!(
        costs.iter().all(|c| *c == costs[0]),
        "leg costs must share a column, got {costs:?}:\n{text}"
    );
    let served: Vec<usize> = legs
        .iter()
        .map(|l| columns(&l[..l.find(" served").expect("a leg reports what it served")]))
        .collect();
    assert!(
        served.iter().all(|s| *s == served[0]),
        "served counts must share a column, got {served:?}:\n{text}"
    );
    // A leg that rejected nothing leaves the cell blank rather than writing a
    // `0` the eye then has to filter out of the column it is scanning.
    assert!(
        !text.contains("0 rejected"),
        "a zero rejection is an empty cell:\n{text}"
    );
}

/// A kind longer than its column is elided into it, never allowed to overrun.
///
/// `kind` is wire text with nothing bounding it upstream, and a soft column —
/// the deliberate posture for a tool name, where identity outranks alignment —
/// would let one unknown provider's kind displace every cell to its right on
/// that row alone.
#[test]
fn a_kind_wider_than_its_column_is_elided_not_overrun() {
    let mut entry = screenshot_recall();
    let TranscriptEntry::ContextRecall { frames, .. } = &mut entry else {
        unreachable!()
    };
    frames[0].kind = "retrieved-document-fragment".into();

    let text = recall_text(&entry, true, 100);
    // Frame rows only: the budget summary and its legs end in `tok` too and
    // belong to the other grid.
    let rows: Vec<usize> = text
        .lines()
        .filter(|l| {
            l.ends_with(" tok") && !l.contains(" served") && !l.trim_start().starts_with("budget ")
        })
        .map(columns)
        .collect();
    assert_eq!(rows.len(), 5, "expected the five frame rows, got {rows:?}");
    assert!(
        rows.iter().all(|w| *w == rows[0]),
        "an over-wide kind must not change a row's width — got {rows:?}:\n{text}"
    );
    assert!(
        !text.contains("retrieved-document-fragment"),
        "the kind is elided into its column:\n{text}"
    );
}

/// `used_ann_index` is tri-state on the wire and stays tri-state on screen.
///
/// A `bool` would render `scan` on every turn a recall path does not report the
/// flag, which reads as "the index never fires" rather than "nobody said".
#[test]
fn an_unreported_ann_flag_renders_as_nothing_not_as_scan() {
    let mut entry = screenshot_recall();
    let TranscriptEntry::ContextRecall { used_ann_index, .. } = &mut entry else {
        unreachable!()
    };
    *used_ann_index = None;
    let text = recall_text(&entry, false, 100);
    assert!(!text.contains("scan"), "{text}");
    assert!(!text.contains(" ann"), "{text}");

    let TranscriptEntry::ContextRecall { used_ann_index, .. } = &mut entry else {
        unreachable!()
    };
    *used_ann_index = Some(false);
    assert!(recall_text(&entry, false, 100).contains("scan"));
}

/// `latency_ms: 0` means *not measured*, so no duration is printed.
#[test]
fn an_unmeasured_recall_latency_is_omitted_not_printed_as_zero() {
    let mut entry = screenshot_recall();
    let TranscriptEntry::ContextRecall { latency_ms, .. } = &mut entry else {
        unreachable!()
    };
    *latency_ms = 0;
    let text = recall_text(&entry, false, 100);
    assert!(!text.contains("0ms"), "{text}");
}
