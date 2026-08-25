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
    StageKind, SteerCause, SubAgentStatus,
};

mod block_rail;
mod inline_diff;
mod mutation_diff_e2e;
mod palette;
mod result_row;
mod slash;
mod steering;
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
        TranscriptEntry::Stage {
            name: StageKind::Execute.into(),
            opens: None,
        },
        TranscriptEntry::Text("ok".into()),
        TranscriptEntry::Reasoning("hmm".into()),
        TranscriptEntry::ToolStart {
            call_id: "c1".into(),
            name: "bash".into(),
            input: "ls".into(),
            raw: "{}".into(),
            path: None,
            sub_agent_id: None,
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
            diff: Vec::new(),
            read_size: None,
            sub_agent_id: None,
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
        TranscriptEntry::SteeringWithheld {
            withheld_by: stella_protocol::Withholder::ProjectUntrusted,
            memories: 3,
            records: 1,
            skills: 0,
            commands: 2,
            agents: 1,
        },
        TranscriptEntry::Complete {
            receipt: Default::default(),
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
        receipt: Default::default(),
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

    // A turn that counted nothing still claims nothing. `det %` is gone from
    // the design outright; tests have no source at all
    // ([`crate::model::TurnCounters`] states what would have to exist); and
    // tokens, files, memories and the clock are counted now but were zero here,
    // which elides identically. A number nobody took never reaches this line.
    for absent in ["tok", "det ", "tests", "file", "memory", "0:00"] {
        assert!(
            !text.contains(absent),
            "the receipt invented {absent:?}:\n{text}"
        );
    }
}

/// SPEC 6.1's receipt, fed. The witness for #4184: every field below names the
/// event it is summed from, and each was absent from this line before the fold
/// counted it.
#[test]
fn the_receipt_reports_what_the_turn_measured() {
    use crate::deck::WorkspaceModel;
    use crate::envelope::{AgentMeta, Inbound};

    let mut deck = WorkspaceModel::new();
    deck.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
    let mut send = |event: AgentEvent| {
        deck.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event,
        });
    };

    // tokens ← StepUsage's token fields (never its cost_usd)
    send(AgentEvent::StepUsage {
        step: 1,
        role: Default::default(),
        provider: "openrouter".into(),
        upstream_provider: None,
        output_text: None,
        model: "glm-5.2".into(),
        input_tokens: 12_000,
        output_tokens: 6_000,
        cached_input_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: None,
        estimated_input_tokens: 0,
        cost_usd: 99.0,
        duration_ms: 0,
        retries: 0,
        tool_calls: 0,
        complete: true,
        finish_reason: None,
        effort: None,
        max_output_tokens: None,
        temperature: None,
        params: None,
        sub_agent_id: None,
    });
    // files ← FileChange, distinct paths
    for path in ["src/a.rs", "src/b.rs", "src/a.rs"] {
        send(AgentEvent::FileChange {
            path: path.into(),
            kind: FileChangeKind::Modified,
            added: 1,
            removed: 0,
            diff: None,
        });
    }
    // memories ← ContextWrite's upserts
    send(AgentEvent::ContextWrite {
        provider: "memory".into(),
        upserts: 2,
        superseded: 0,
    });
    send(AgentEvent::TurnComplete {
        model: "glm-5.2".into(),
        cost_usd: 0.11,
    });

    let entry = deck.agents[0]
        .model
        .transcript
        .iter()
        .rev()
        .find(|e| matches!(e, TranscriptEntry::Complete { .. }))
        .expect("the turn closed");
    let text = recall_text(entry, false, 120);

    assert!(text.contains("18k tok"), "tokens not summed:\n{text}");
    assert!(text.contains("2 files"), "files not counted:\n{text}");
    assert!(text.contains("2 memories"), "memories not counted:\n{text}");
    // The cost is the turn's, never StepUsage's — folding that would
    // double-count the spend BudgetTick already drives.
    assert!(text.contains("$0.11"), "{text}");
    assert!(
        !text.contains("$99"),
        "usage cost leaked into the receipt:\n{text}"
    );
    // Still no source, still absent.
    assert!(
        !text.contains("tests"),
        "the receipt invented a test tally:\n{text}"
    );
}

/// The elapsed is the one receipt field the fold may not measure (L-T1), so it
/// is stamped by the deck on the way past. The witness is that it arrives.
#[test]
fn the_closing_rule_carries_the_turn_clock() {
    use crate::deck::WorkspaceModel;
    use crate::envelope::{AgentMeta, Inbound};

    let mut deck = WorkspaceModel::new();
    deck.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
    deck.now_ms = 1_000;
    deck.apply_inbound(&Inbound::PromptStarted {
        agent: "lead".into(),
        text: "do the thing".into(),
    });
    deck.now_ms = 8_500;
    deck.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::TurnComplete {
            model: "glm-5.2".into(),
            cost_usd: 0.11,
        },
    });

    let entry = deck.agents[0]
        .model
        .transcript
        .iter()
        .rev()
        .find(|e| matches!(e, TranscriptEntry::Complete { .. }))
        .expect("the turn closed");
    let TranscriptEntry::Complete { receipt, .. } = entry else {
        unreachable!()
    };
    assert_eq!(
        receipt.elapsed_ms,
        Some(7_500),
        "the deck did not stamp the turn clock onto the receipt"
    );
    assert!(
        recall_text(entry, false, 120).contains("7"),
        "the closing rule dropped the elapsed"
    );
}

/// A turn that dies without a `TurnComplete` must not bill its tokens to the
/// next turn's receipt.
#[test]
fn counters_reset_at_the_turn_boundary() {
    use crate::model::SessionModel;

    let mut sm = SessionModel::default();
    sm.apply(&AgentEvent::ContextWrite {
        provider: "memory".into(),
        upserts: 5,
        superseded: 0,
    });
    assert_eq!(sm.turn_counters.memories, 5);
    sm.push_user_prompt("a new turn");
    assert_eq!(sm.turn_counters.memories, 0);
}

/// SPEC 6.1: a turn opens on a labelled rule naming the turn, its stage, the
/// model and the budget in force.
///
/// The witness for the opening boundary landing. On the old renderer a stage
/// was a hairline carrying the stage word alone, so every assertion below fails
/// on it: no `turn 4`, no model, no budget, and — the point of the rule —
/// nothing tying the events under it to the receipt that closes them.
#[test]
fn a_turn_opens_on_a_labelled_rule() {
    let entry = TranscriptEntry::Stage {
        name: StageKind::Execute.into(),
        opens: Some(crate::model::TurnOpening {
            turn: 4,
            model: Some("kimi-k3".into()),
            budget_usd: Some(0.60),
            queued_steer: None,
        }),
    };
    let text = recall_text(&entry, false, 100);

    assert!(text.contains("turn 4"), "no opening rule:\n{text}");
    assert!(text.contains("execute"), "the rule lost its stage:\n{text}");
    assert!(text.contains("kimi-k3"), "the rule lost its model:\n{text}");
    assert!(
        text.contains("budget $0.60"),
        "the rule lost its budget:\n{text}"
    );
    assert!(
        text.contains("──"),
        "the rule does not reach the row:\n{text}"
    );
}

/// …and states only what the session actually knows.
///
/// The first turn of a session has no model — `Hud::model` is fed by
/// `TurnComplete`, which has not arrived — and a run with no budget armed has
/// no ceiling. Both elide. The failure this guards is not cosmetic: a rule that
/// filled the gaps would open the transcript by naming a model nobody routed to
/// and a `$0.00` nobody set, on the row a reader trusts to say what this turn
/// is (#4183).
#[test]
fn an_opening_rule_names_no_model_or_budget_it_was_never_told() {
    let text = recall_text(
        &TranscriptEntry::Stage {
            name: StageKind::Execute.into(),
            opens: Some(crate::model::TurnOpening {
                turn: 1,
                model: None,
                budget_usd: None,
                queued_steer: None,
            }),
        },
        false,
        100,
    );
    assert!(text.contains("turn 1 execute"), "{text}");
    for absent in ["budget", "$0.00", "$"] {
        assert!(
            !text.contains(absent),
            "the opening rule invented {absent:?}:\n{text}"
        );
    }
}

/// SPEC 6.1 draws **one** labelled rule per turn, and SPEC 2 makes the turn the
/// transcript's unit. A later stage of the same turn stays the plain section
/// rule it has always been — otherwise a wrapped run with triage, plan, execute
/// and verify inside one turn would announce `turn 4` four times and the number
/// would stop reading as the turn's identity.
#[test]
fn a_stage_inside_a_turn_is_not_a_second_turn_rule() {
    let text = recall_text(
        &TranscriptEntry::Stage {
            name: StageKind::Verify.into(),
            opens: None,
        },
        false,
        100,
    );
    // The long-form section rule sets its label in caps; what matters here is that the
    // stage still names itself and that no turn number rides beside it.
    assert!(
        text.to_ascii_lowercase().contains("verify"),
        "the section rule vanished:\n{text}"
    );
    assert!(
        !text.to_ascii_lowercase().contains("turn "),
        "a second turn rule:\n{text}"
    );
}

/// SPEC 6.1 end to end, over the live path: events fold into a
/// [`SessionModel`], the model's entries render through [`entry_lines`], and
/// the result is a turn wrapped in its two rules with the receipt beneath.
///
/// This is #4124's definition of done — that
/// `design/tui-v2/renderings/png/01-session-turn-lifecycle.png` is reproducible
/// as a *live screen* and not merely as a call to the pure renderers. The unit
/// tests above each prove one row from a hand-built entry; only this one proves
/// that the fold stamps what the renderer needs and that the router reaches it.
/// It asserts order, because a receipt above its own turn rule would be a
/// correct set of rows and a wrong transcript.
#[test]
fn a_folded_turn_renders_its_whole_lifecycle_in_order() {
    use stella_protocol::{StageScope, ToolCall};

    let mut model = crate::model::SessionModel::new();
    for event in [
        AgentEvent::Stage {
            name: StageKind::Execute.into(),
            scope: StageScope::Run,
        },
        AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "c1".into(),
                name: "read_file".into(),
                input: serde_json::json!({ "path": "src/lifecycle.rs" }),
            },
            sub_agent_id: None,
        },
        AgentEvent::TurnComplete {
            model: "kimi-k3".into(),
            cost_usd: 0.11,
        },
    ] {
        model.apply(&event);
    }

    let mut out = Vec::new();
    for entry in &model.transcript {
        entry_lines(
            entry,
            EntryView::default(),
            false,
            false,
            false,
            100,
            &mut out,
        );
    }
    let rows: Vec<String> = out
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    let index_of = |needle: &str| {
        rows.iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("no row containing {needle:?} in:\n{}", rows.join("\n")))
    };

    let begin = index_of("turn 1 execute");
    let event = index_of("src/lifecycle.rs");
    let end = index_of("turn 1 done");
    let receipt = index_of("receipt");
    assert!(
        begin < event && event < end && end < receipt,
        "the turn's rows are out of order (begin {begin}, event {event}, end {end}, \
         receipt {receipt}):\n{}",
        rows.join("\n")
    );
}

/// SPEC 2: money is gold. The row used to render a settled turn cost in
/// `SUCCESS_BRIGHT` green, which spends the pass colour on an amount — and
/// green is reserved for pass semantics, not for spending.
#[test]
fn the_turn_receipt_prices_in_gold_not_in_the_pass_colour() {
    let mut out = Vec::new();
    entry_lines(
        &TranscriptEntry::Complete {
            receipt: Default::default(),
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

/// Fold a turn that opens and is then steered, and hand back what the opening
/// rule renders on the live path — `SessionModel::apply` → `entry_lines`, not
/// a hand-built [`crate::model::TurnOpening`].
///
/// The renderer half has been covered since #4123
/// (`views::transcript::tests::a_consumed_steer_is_named_on_the_turn_rule`); what
/// was missing, and what the two tests below are, is the producer.
fn opening_rule_of_a_steered_turn(cause: SteerCause) -> String {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::Stage {
        name: StageKind::Execute.into(),
        scope: stella_protocol::StageScope::Run,
    });
    model.apply(&AgentEvent::Steered {
        text: "also fix the flake".into(),
        cause,
    });
    let entry = model
        .transcript
        .iter()
        .find(|e| matches!(e, TranscriptEntry::Stage { opens: Some(_), .. }))
        .expect("the turn opened on a rule");
    recall_text(entry, false, 120)
}

/// SPEC 6.1: the steer a turn consumed is named on that turn's opening rule.
///
/// This is the payoff the composer's `⏎ queue (never blocks)` promises, and it
/// is the witness for #4185. Before it, `turn_begin_rows` passed
/// `queued_steer: None` unconditionally — the renderer worked and nothing on
/// the live path could ever reach it, so the label was a field with no
/// producer.
///
/// Back-filled after the rule is drawn, exactly as `TurnOpening::model` is
/// (#4183): a steer lands mid-turn, so there is nothing to stamp at the
/// boundary.
#[test]
fn a_user_steer_is_named_on_the_rule_of_the_turn_that_consumed_it() {
    let text = opening_rule_of_a_steered_turn(SteerCause::User);
    assert!(
        text.contains("queued: \"also fix the flake\""),
        "the rule did not name the steer this turn consumed:\n{text}"
    );
}

/// …and **only** a person's steer.
///
/// The engine's two automatic rungs emit the same `AgentEvent::Steered`, and a
/// rule labelling a stall-rung auto-steer as something the user queued would be
/// worse than the blank it replaces — which is exactly why #4185 sat blocked on
/// #3622 rather than guessing.
///
/// `Unknown` is in the sweep for the same reason: a session recorded before the
/// cause existed must keep its blank rather than be attributed to a person.
///
/// This is the half that would pass trivially against a fix that never fed the
/// label at all, so it is evidence only when read beside its twin above.
#[test]
fn an_engine_authored_steer_leaves_the_turn_rule_blank() {
    for cause in [SteerCause::Loop, SteerCause::Stall, SteerCause::Unknown] {
        let text = opening_rule_of_a_steered_turn(cause);
        assert!(
            !text.contains("queued:"),
            "{cause:?} is not the user speaking, and the rule said it was:\n{text}"
        );
    }
}

/// The steer keeps its own `(steered mid-turn)` transcript row either way.
///
/// The rule says what the turn opened by consuming; the row says when it
/// landed. Dropping the row in favour of the label would lose the position,
/// and the two rungs that get no label would lose their record entirely.
#[test]
fn a_steer_keeps_its_own_row_whatever_its_cause() {
    for cause in [SteerCause::User, SteerCause::Loop] {
        let mut model = SessionModel::new();
        model.apply(&AgentEvent::Stage {
            name: StageKind::Execute.into(),
            scope: stella_protocol::StageScope::Run,
        });
        model.apply(&AgentEvent::Steered {
            text: "also fix the flake".into(),
            cause,
        });
        assert!(
            model.transcript.iter().any(|e| matches!(
                e,
                TranscriptEntry::User(t) if t.starts_with("(steered mid-turn)")
            )),
            "{cause:?} lost its transcript row"
        );
    }
}
