use super::*;
use crate::composer::{Composer, SlashCommand};
// Imported here rather than by the parent: `render.rs` itself no longer names
// these once the transcript builders moved to `render::entry`, and re-adding
// them there purely for the test module would be an unused import in the lib.
use crate::model::TranscriptEntry;
use proptest::prelude::*;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use stella_protocol::{
    AgentEvent, BudgetMode, FileChangeKind, MediaJobState, MediaKind, ScopeProposal, StageKind,
    ToolCall, ToolOutput,
};
use stella_protocol::{CiStatus, PrStatus};

mod thinking;

/// Flatten a `TestBackend` buffer to one `String` per row (styling
/// stripped — content is what we assert on, never raw ANSI, per L-T6).
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

fn draw(model: &SessionModel, ui: &mut UiState, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| render(model, ui, f)).unwrap();
    buffer_text(terminal.backend().buffer())
}

#[test]
fn hud_and_transcript_render_the_event_content() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::Stage {
        name: StageKind::Execute,
    });
    model.apply(&AgentEvent::Text {
        delta: "building the thing".into(),
    });
    model.apply(&AgentEvent::Complete {
        model: "glm-5.2".into(),
        cost_usd: 0.0123,
    });
    let mut ui = UiState::default();
    let text = draw(&model, &mut ui, 100, 30);
    assert!(text.contains("glm-5.2"), "HUD shows the model:\n{text}");
    assert!(
        text.contains("building the thing"),
        "transcript shows text:\n{text}"
    );
    assert!(text.contains("complete"), "shows completion:\n{text}");
}

/// One of every [`TranscriptEntry`] variant that renders as an entry, for the
/// tests that must cover the whole enum. `Evicted` is deliberately absent —
/// it is a note *about* the transcript rather than an entry in it.
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
        TranscriptEntry::JudgeVerdict {
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
    ]
}

/// Every transcript entry opens on a rail, so the left margin stays a
/// scannable index column rather than a ragged edge: a reader running their
/// eye down column 0 sees the *shape* of the session — calls, outcomes,
/// prompts, system notes — before reading a single word.
///
/// The invariant is about the margin carrying a glyph, not about the prefix's
/// width. A system note's prefix (`↻ retry  `, `⇣ compacted  `) is far wider
/// than [`BODY`] and that is correct: the label *is* the note's rail, opening
/// with its own glyph in column 0. What must never happen is a row that
/// begins with bare unstyled text at the left margin.
#[test]
fn every_transcript_entry_renders_on_a_rail() {
    let samples = sample_entries();
    for entry in &samples {
        // Exhaustive on purpose: adding a `TranscriptEntry` variant fails
        // to compile here — add a sample above and render the new arm
        // through `push_row`/`push_row_block` (a rail) or `push_note` (a
        // system note, whose label carries its own glyph).
        match entry {
            TranscriptEntry::User(_)
            | TranscriptEntry::Stage(_)
            | TranscriptEntry::Text(_)
            | TranscriptEntry::Reasoning(_)
            | TranscriptEntry::ToolStart { .. }
            | TranscriptEntry::ToolResult { .. }
            | TranscriptEntry::Retry { .. }
            | TranscriptEntry::Compaction { .. }
            // Not in `samples`: it is a note *about* the transcript rather
            // than an entry in it, so it renders untagged and full-bleed —
            // see `eviction_marker_renders_as_a_one_line_system_note`.
            | TranscriptEntry::Evicted { .. }
            | TranscriptEntry::BudgetTick { .. }
            | TranscriptEntry::ProviderFallback { .. }
            | TranscriptEntry::ContextRecall { .. }
            | TranscriptEntry::ContextWrite { .. }
            | TranscriptEntry::MediaProgress { .. }
            | TranscriptEntry::MediaComplete { .. }
            | TranscriptEntry::JudgeVerdict { .. }
            | TranscriptEntry::GoalVerdict { .. }
            | TranscriptEntry::ScopeReview { .. }
            | TranscriptEntry::AskUser { .. }
            | TranscriptEntry::Commit { .. }
            | TranscriptEntry::Pr { .. }
            | TranscriptEntry::TaskUpdate { .. }
            | TranscriptEntry::Error { .. }
            | TranscriptEntry::Complete { .. } => {}
        }
        let mut lines = Vec::new();
        entry_lines(entry, &[], false, false, false, 0, &mut lines);
        let first = lines
            .first()
            .unwrap_or_else(|| panic!("{entry:?} renders no lines"));
        let rail = first.spans.first().expect("first span is the rail prefix");
        let prefix = rail.content.as_ref();
        assert!(
            !prefix.is_empty(),
            "{entry:?} renders with no rail prefix at all"
        );
        let opens_in_column_zero = !prefix.starts_with(' ');
        // A tool result and its body are subordinate to the call above them,
        // so they indent one rail-width and carry their glyph at column 2.
        // These two prefixes are the *only* legal indented rails.
        let subordinate = prefix == Rail::Result.prefix() || prefix == Rail::Fail.prefix();
        // Assistant prose is the deliberate exception: `Rail::Agent` is two
        // bare spaces. Prose is the transcript's default voice, and a marker
        // on every paragraph would be noise in the one place a reader is
        // reading words rather than scanning the margin — so it indents to
        // the content column and shows nothing.
        let agent_prose = prefix == Rail::Agent.prefix();
        assert!(
            opens_in_column_zero || subordinate || agent_prose,
            "{entry:?} must open on a rail — a glyph in column 0, the \
             subordinate `{:?}`/`{:?}` result rails, or assistant prose — \
             never bare text at the left margin; got {prefix:?}",
            Rail::Result.prefix(),
            Rail::Fail.prefix(),
        );
        if opens_in_column_zero {
            let glyph = prefix.chars().next().expect("non-empty prefix");
            assert!(
                !glyph.is_whitespace() && !glyph.is_alphanumeric(),
                "{entry:?} must index the margin with a glyph, not a letter: {prefix:?}"
            );
        }
    }
}

/// A wrapped continuation line begins flush at the content column: exactly
/// the rail's indent in leading spaces, never one more. Regression for the bug where
/// the wrap-boundary space was carried onto the next line, stacking on top
/// of the indent and drifting every wrapped row one column right of the
/// clean left edge (the "extra blank space after the colon on wrap" report).
#[test]
fn wrapped_continuation_starts_flush_at_the_content_column() {
    let content = "the quick brown fox jumps over the lazy dog and then keeps \
                   on running well past the right edge to force several wraps";
    let spans = vec![
        Span::raw(Rail::Result.prefix().to_string()),
        Span::raw(content),
    ];
    let mut out = Vec::new();
    // Narrow width so the content wraps several times.
    wrap_one_indent(Line::from(spans), 60, BODY, &mut out);

    assert!(
        out.len() > 1,
        "content must wrap into a continuation row, got {} row(s)",
        out.len()
    );
    for (i, line) in out.iter().enumerate().skip(1) {
        let text: String = line.spans.iter().flat_map(|s| s.content.chars()).collect();
        let leading = text.chars().take_while(|c| *c == ' ').count();
        assert_eq!(
            leading, BODY,
            "continuation row {i} must start exactly at the content column \
             (indent {BODY}, no carried wrap space); got {leading}: {text:?}",
        );
    }
}

/// The collapsed-result anchor: the first failure-marked line wins over the
/// first line, a mid-sentence "error" does not hijack the row, and the
/// marker-with-colon form only counts near the start of the line.
#[test]
fn salient_line_anchors_on_failure_markers_not_prose() {
    // A build log: the error is what the collapsed row must show.
    assert_eq!(
        salient_line("Checking foo v0.1.0\nerror[E0432]: unresolved import"),
        1
    );
    assert_eq!(salient_line("warning: unused variable `x`"), 0);
    // `path: error:` within the 12-column head window still counts…
    assert_eq!(salient_line("ok\nsrc/a.rs:3: error: boom"), 1);
    // …but prose that merely mentions error handling does not.
    assert_eq!(salient_line("we improved error handling here\nfine"), 0);
    // No marker: the first non-blank line is the anchor.
    assert_eq!(salient_line("\n\n  hello"), 2);
    assert_eq!(salient_line(""), 0);
}

/// The unit follows the magnitude: ms below a second, one decimal below ten
/// seconds, whole seconds below a minute, m/s above it.
#[test]
fn human_duration_scales_its_unit_with_the_magnitude() {
    assert_eq!(human_duration(999), "999ms");
    assert_eq!(human_duration(1_000), "1.0s");
    assert_eq!(human_duration(4_210), "4.2s");
    assert_eq!(human_duration(12_000), "12s");
    assert_eq!(human_duration(59_999), "59s");
    assert_eq!(human_duration(60_000), "1m00s");
    assert_eq!(human_duration(125_000), "2m05s");
}

#[test]
fn thousands_groups_digits_and_plural_lines_reads_as_english() {
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(1_000), "1,000");
    assert_eq!(thousands(1_234_567), "1,234,567");
    assert_eq!(plural_lines(1), "1 line");
    assert_eq!(plural_lines(1_584), "1,584 lines");
}

/// `left … right` layout: padded to the pane at ordinary widths, the left
/// truncated (ellipsis) rather than wrapped when the two would collide.
#[test]
fn justify_pads_to_width_and_truncates_the_left_on_collision() {
    let flat =
        |spans: &[Span<'_>]| -> String { spans.iter().map(|s| s.content.as_ref()).collect() };
    let row = justify(vec![Span::raw("ab")], vec![Span::raw("cd")], 20, 0);
    let text = flat(&row);
    assert_eq!(text.len(), 20, "metric flush to the pane edge: {text:?}");
    assert!(text.starts_with("ab") && text.ends_with("cd"));

    // Too narrow for both: the elastic left gives way, ending in `…`,
    // and the fixed-width metric keeps its column.
    let row = justify(vec![Span::raw("abcdef")], vec![Span::raw("xy")], 8, 0);
    assert_eq!(flat(&row), "abcd… xy");
}

#[test]
fn files_panel_lists_touched_files_by_label() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::FileChange {
        path: "src/driver.rs".into(),
        kind: FileChangeKind::Modified,
        added: 1,
        removed: 1,
        diff: Some("@@\n-old\n+new".into()),
    });
    let mut ui = UiState::default();
    let text = draw(&model, &mut ui, 100, 20);
    assert!(text.contains("src/driver.rs"), "files panel:\n{text}");
    assert!(text.contains("files touched"), "panel title:\n{text}");
}

#[test]
fn diff_viewer_shows_the_selected_files_diff() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::FileChange {
        path: "a.rs".into(),
        kind: FileChangeKind::Modified,
        added: 1,
        removed: 1,
        diff: Some("@@ -1 +1 @@\n-removed\n+added".into()),
    });
    let mut ui = UiState::default();
    ui.diff_open = true;
    let text = draw(&model, &mut ui, 100, 20);
    assert!(text.contains("removed"), "diff shows removals:\n{text}");
    assert!(text.contains("added"), "diff shows additions:\n{text}");
    // The PR-style chrome: the path rides the top rule, the line counts
    // ride the bottom rule.
    assert!(text.contains("a.rs"), "path in the header rule:\n{text}");
    assert!(
        text.contains("+1 addition") && text.contains("-1 removal"),
        "counts in the footer rule:\n{text}"
    );
}

#[test]
fn scope_card_renders_the_decision_legend_when_unanswered() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::ScopeReview {
        proposal: ScopeProposal {
            summary: "refactor auth".into(),
            steps: vec!["s1".into(), "s2".into()],
            estimated_files: 9,
            estimated_cost_usd: Some(1.5),
        },
    });
    let mut ui = UiState::default();
    let text = draw(&model, &mut ui, 100, 30);
    assert!(text.contains("refactor auth"), "card summary:\n{text}");
    assert!(text.contains("pprove"), "shows approve legend:\n{text}");
    // Once answered, the legend flips to the awaiting message.
    ui.scope_answered = true;
    let text2 = draw(&model, &mut ui, 100, 30);
    assert!(text2.contains("awaiting"), "flips to awaiting:\n{text2}");
}

#[test]
fn ask_user_card_always_offers_a_free_text_affordance() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::AskUser {
        id: "q1".into(),
        question: "which database?".into(),
        options: vec!["postgres".into(), "sqlite".into()],
    });
    let mut ui = UiState::default();
    let text = draw(&model, &mut ui, 100, 30);
    assert!(text.contains("which database?"), "question:\n{text}");
    assert!(text.contains("postgres"), "option 1:\n{text}");
    assert!(text.contains("sqlite"), "option 2:\n{text}");
    // The binding renderer contract: a free-text affordance every time.
    assert!(
        text.contains("type your own answer"),
        "free-text option:\n{text}"
    );
}

#[test]
fn slash_menu_renders_filtered_commands() {
    let model = SessionModel::new();
    let mut composer = Composer::new();
    for c in "/di".chars() {
        composer.insert_char(c);
    }
    let mut ui = UiState::new(
        composer,
        vec![
            SlashCommand::new("/diff", "open the diff viewer"),
            SlashCommand::new("/files", "focus files"),
        ],
    );
    let text = draw(&model, &mut ui, 100, 30);
    assert!(text.contains("/diff"), "slash menu lists /diff:\n{text}");
    assert!(!text.contains("/files"), "filtered out /files:\n{text}");
}

#[test]
fn slash_popup_marks_the_selected_command() {
    let model = SessionModel::new();
    let mut composer = Composer::new();
    composer.insert_char('/');
    let mut ui = UiState::new(
        composer,
        vec![
            SlashCommand::new("/help", "show help"),
            SlashCommand::new("/diff", "open the diff viewer"),
        ],
    );
    ui.slash_selected = 1;
    let text = draw(&model, &mut ui, 100, 30);
    // The glyph is double-width: its trailing filler cell dumps as an
    // extra space before the explicit separator.
    assert!(
        text.contains("▸ 🔒  /diff"),
        "selection marker + builtin glyph on the second row:\n{text}"
    );
    assert!(
        text.contains("/ commands · 2"),
        "popup title with the match count:\n{text}"
    );
}

#[test]
fn slash_popup_glyphs_distinguish_builtin_from_custom_commands() {
    let model = SessionModel::new();
    let mut composer = Composer::new();
    composer.insert_char('/');
    let mut ui = UiState::new(
        composer,
        vec![
            SlashCommand::new("/help", "show help"),
            SlashCommand::custom("/fix-bug", "fix the named bug"),
        ],
    );
    let text = draw(&model, &mut ui, 100, 30);
    assert!(
        text.contains("🔒  /help"),
        "productized commands carry the lock glyph:\n{text}"
    );
    assert!(
        text.contains("⚡  /fix-bug"),
        "custom commands carry the lightning glyph:\n{text}"
    );
}

// ---- Slash-popup windowing ---------------------------------------------

#[test]
fn scroll_window_start_holds_still_until_the_selection_leaves_the_edge() {
    // Fits entirely: never scrolls.
    assert_eq!(scroll_window_start(5, 4, 8), 0);
    // Selection inside the first window: no movement.
    assert_eq!(scroll_window_start(20, 0, 8), 0);
    assert_eq!(scroll_window_start(20, 7, 8), 0);
    // One past the window's last row: scroll down by one.
    assert_eq!(scroll_window_start(20, 8, 8), 1);
    // The tail clamps so the final window is full, never blank-padded.
    assert_eq!(scroll_window_start(20, 19, 8), 12);
    // Selecting back at the top pulls the window all the way up.
    assert_eq!(scroll_window_start(20, 0, 8), 0);
    // A stale selection past the end (e.g. the filter just shrank the
    // match list) clamps to the last full window instead of panicking.
    assert_eq!(scroll_window_start(20, 999, 8), 12);
    // Degenerate inputs don't panic.
    assert_eq!(scroll_window_start(0, 0, 8), 0);
    assert_eq!(scroll_window_start(5, 0, 0), 0);
}

/// Rendering a slash popup taller than its window keeps the *selected*
/// row on screen and pushes the ones scrolled past off it — the concrete
/// symptom of the un-windowed version (selection navigable but invisible).
#[test]
fn slash_popup_windows_the_selection_into_view() {
    let cmds: Vec<SlashCommand> = (0..15)
        .map(|i| SlashCommand::new(format!("/cmd{i:02}"), "desc"))
        .collect();
    let menu = SlashMenu::filter(&cmds, "/");
    let area = Rect {
        x: 0,
        y: 0,
        width: 56,
        height: (SLASH_POPUP_MAX_ROWS as u16) + 3,
    };
    // Select the very last command: without windowing it renders off the
    // bottom of the popup box and never appears in the buffer.
    let mut buf = Buffer::empty(area);
    render_slash_popup(&menu, 14, area, &mut buf);
    let text = buffer_text(&buf);
    assert!(text.contains("/cmd14"), "selected row is visible:\n{text}");
    assert!(
        !text.contains("/cmd00"),
        "the top rows scrolled out of view:\n{text}"
    );
    // The legend advertises the hidden rows above.
    assert!(text.contains('▲'), "scroll affordance shown:\n{text}");

    // Selecting the top shows the head and hides the tail instead.
    let mut buf = Buffer::empty(area);
    render_slash_popup(&menu, 0, area, &mut buf);
    let text = buffer_text(&buf);
    assert!(text.contains("/cmd00"), "top row visible:\n{text}");
    assert!(!text.contains("/cmd14"), "tail hidden:\n{text}");
    assert!(text.contains('▼'), "hidden-below affordance shown:\n{text}");
}

/// A stale-high selection (the match list shrank under the cursor before
/// the upstream clamp caught up) must still render a sane, in-bounds
/// window rather than panic on the slice.
#[test]
fn slash_popup_survives_a_selection_past_the_filtered_end() {
    let cmds: Vec<SlashCommand> = (0..3)
        .map(|i| SlashCommand::new(format!("/cmd{i:02}"), "desc"))
        .collect();
    let menu = SlashMenu::filter(&cmds, "/");
    let area = Rect {
        x: 0,
        y: 0,
        width: 56,
        height: (SLASH_POPUP_MAX_ROWS as u16) + 3,
    };
    let mut buf = Buffer::empty(area);
    // selected far past the 3 matches — the render-side clamp keeps it in
    // view; all three rows fit so nothing scrolls.
    render_slash_popup(&menu, 99, area, &mut buf);
    let text = buffer_text(&buf);
    assert!(text.contains("/cmd02"), "last row still shown:\n{text}");
    assert!(
        !text.contains('▲') && !text.contains('▼'),
        "short list shows no scroll affordance:\n{text}"
    );
}

#[test]
fn transcript_scrolls_line_exact_to_show_the_tail() {
    let mut model = SessionModel::new();
    for i in 0..200 {
        model.apply(&AgentEvent::Text {
            delta: format!("LINE{i:03}\n"),
        });
    }
    // The trailing streaming text is one entry with 200 embedded newlines
    // → 201 visual lines; following must land on the last of them.
    let mut ui = UiState::default();
    let text = draw(&model, &mut ui, 80, 20);
    assert!(
        text.contains("LINE199"),
        "tail is visible while following:\n{text}"
    );
    assert!(!text.contains("LINE000"), "head is scrolled off:\n{text}");
}

#[test]
fn ui_transcript_cache_follows_file_mutations_and_keeps_each_results_own_diff() {
    use stella_protocol::FileChangeKind as FK;
    // A FileChange appends NOTHING to the transcript — every older
    // fingerprint term (entry count, tail lengths, width) holds still —
    // yet it can change what a *settled* tool result renders. Only the
    // file-mutation term can catch that.
    //
    // What it changes moved with `FileState::recent_diffs`: a single later
    // mutation no longer stales anything, because each result resolves the
    // diff recorded at its own seq and the path remembers the last
    // `DIFF_HISTORY` of them. A row therefore keeps showing the change it
    // made instead of blanking the moment the file is touched again. Only
    // once its diff is *evicted* from that history does the row lose it —
    // and the cache has to notice, still with nothing appended.
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c1".into(),
            name: "edit_file".into(),
            input: serde_json::json!({"path": "src/x.rs"}),
        },
    });
    model.apply(&AgentEvent::FileChange {
        path: "src/x.rs".into(),
        kind: FK::Modified,
        added: 1,
        removed: 0,
        diff: Some("@@ -1,1 +1,1 @@\n+first_diff_line".into()),
    });
    model.apply(&AgentEvent::ToolResult {
        call_id: "c1".into(),
        output: ToolOutput::Ok {
            content: "ok".into(),
        },
        duration_ms: 3,
        speculated: false,
    });
    let mut ui = UiState::default();
    ui.ensure_transcript_lines(&model, false, 120);
    let text = |lines: &[Line<'_>]| -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.clone()))
            .collect()
    };
    assert!(
        text(ui.transcript_lines()).contains("first_diff_line"),
        "the fresh inline diff renders"
    );

    let len_before = model.transcript.len();
    model.apply(&AgentEvent::FileChange {
        path: "src/x.rs".into(),
        kind: FK::Modified,
        added: 1,
        removed: 0,
        diff: Some("@@ -1,1 +1,1 @@\n+second_diff_line".into()),
    });
    assert_eq!(model.transcript.len(), len_before, "no transcript append");
    ui.ensure_transcript_lines(&model, false, 120);
    let after = text(ui.transcript_lines());
    assert!(
        after.contains("first_diff_line"),
        "the settled row keeps the diff IT produced: {after}"
    );
    assert!(
        !after.contains("second_diff_line"),
        "a newer change is never misattributed to it: {after}"
    );

    // Keep mutating until the recorded seq falls off the end of the
    // history. The render must now drop the diff — and the cache must
    // rebuild to show that, with the transcript still untouched.
    for i in 0..crate::model::DIFF_HISTORY {
        model.apply(&AgentEvent::FileChange {
            path: "src/x.rs".into(),
            kind: FK::Modified,
            added: 0,
            removed: 0,
            diff: Some(format!("@@ -1,1 +1,1 @@\n+evicting_edit_{i}")),
        });
    }
    assert_eq!(model.transcript.len(), len_before, "still no append");
    ui.ensure_transcript_lines(&model, false, 120);
    let evicted = text(ui.transcript_lines());
    assert!(
        !evicted.contains("first_diff_line"),
        "a forgotten diff stops rendering: {evicted}"
    );
    assert!(
        !evicted.contains("evicting_edit_"),
        "and is not replaced by a change the call never made: {evicted}"
    );
}

#[test]
fn ui_memoizes_transcript_lines_and_invalidates_on_a_streaming_delta() {
    // The transcript re-wrap is O(transcript) and ran EVERY frame — a
    // session redraws far more often than it changes. The UiState cache
    // must (a) reuse the parsed lines on an unchanged frame, and (b) still
    // invalidate when a streaming delta grows the trailing entry (its
    // length changes but the entry count does not), or new tokens would
    // never appear.
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::Text {
        delta: "a fairly long assistant message that wraps at a narrow width".into(),
    });
    let mut ui = UiState::default();

    // First frame populates the cache, and it matches a direct render.
    ui.ensure_transcript_lines(&model, false, 40);
    let first = ui.transcript_lines().to_vec();
    let ptr = ui.transcript_lines().as_ptr();
    assert_eq!(first, transcript_lines(&model, false, 40));

    // An unchanged frame reuses the SAME backing allocation — no re-wrap.
    ui.ensure_transcript_lines(&model, false, 40);
    assert_eq!(
        ui.transcript_lines().as_ptr(),
        ptr,
        "an unchanged frame must not rebuild the transcript"
    );

    // A streaming delta coalesces into the trailing Text entry: entry count
    // stays 1, but the tail grows. The cache must rebuild and show it.
    model.apply(&AgentEvent::Text {
        delta: " …and still more streamed text arriving token by token".into(),
    });
    assert_eq!(
        model.transcript.len(),
        1,
        "the delta coalesced, not appended"
    );
    ui.ensure_transcript_lines(&model, false, 40);
    assert_eq!(ui.transcript_lines(), transcript_lines(&model, false, 40));
    assert_ne!(
        ui.transcript_lines(),
        first.as_slice(),
        "a grown trailing entry must produce fresh lines"
    );

    // A width change (a resize) also invalidates.
    let wide = ui.transcript_lines().as_ptr();
    ui.ensure_transcript_lines(&model, false, 20);
    assert_ne!(
        ui.transcript_lines().as_ptr(),
        wide,
        "a wrap-width change must rebuild"
    );
    assert_eq!(ui.transcript_lines(), transcript_lines(&model, false, 20));
}

#[test]
fn streaming_preview_renders_live_and_the_authoritative_text_leaves_no_duplicate() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::TextDelta {
        text: "streamed toke".into(),
    });
    model.apply(&AgentEvent::TextDelta {
        text: "ns arriving".into(),
    });
    let mut ui = UiState::default();
    let text = draw(&model, &mut ui, 140, 12);
    assert!(
        text.contains("streamed tokens arriving"),
        "the preview is visible before any Text event lands:\n{text}"
    );

    // A further delta grows the preview WITHOUT changing the entry count
    // — the memoized lines must still invalidate and show it.
    model.apply(&AgentEvent::TextDelta {
        text: " token by token".into(),
    });
    let text = draw(&model, &mut ui, 140, 12);
    assert!(
        text.contains("arriving token by token"),
        "a grown preview must re-render:\n{text}"
    );

    // The step commits: bookkeeping lands, then the authoritative Text
    // replaces the preview — the answer must appear exactly once.
    model.apply(&AgentEvent::BudgetTick {
        spent_usd: 0.01,
        limit_usd: None,
        mode: BudgetMode::Observed,
        session_spent_usd: None,
        session_limit_usd: None,
    });
    model.apply(&AgentEvent::Text {
        delta: "streamed tokens arriving token by token".into(),
    });
    let text = draw(&model, &mut ui, 140, 12);
    assert_eq!(
        text.matches("streamed tokens arriving token by token")
            .count(),
        1,
        "replaced, never duplicated:\n{text}"
    );
}

#[test]
fn a_panicking_panel_becomes_an_error_card_and_input_stays_alive() {
    // L-T7: force a panel to panic via a panicking draw closure and prove
    // (a) it renders as a visible error card, (b) a sibling panel still
    // renders normally, and (c) the pure input path still processes keys.
    let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
    terminal
        .draw(|f| {
            let cols = Layout::horizontal([Constraint::Percentage(50); 2]).split(f.area());
            guarded_panel(f, cols[0], "boom", |_buf| panic!("kaboom in a panel"));
            guarded_panel(f, cols[1], "ok", |buf| {
                Paragraph::new("still-alive")
                    .block(Block::default().borders(Borders::ALL))
                    .render(cols[1], buf);
            });
        })
        .unwrap();
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("panicked"), "error card is visible:\n{text}");
    assert!(
        text.contains("kaboom"),
        "carries the panic message:\n{text}"
    );
    assert!(
        text.contains("still-alive"),
        "sibling panel unaffected:\n{text}"
    );

    // Input handling is entirely independent of rendering and keeps
    // working — the app did not die.
    use crate::ui::{ShellAction, handle_key};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let model = SessionModel::new();
    let mut ui = UiState::default();
    let action = handle_key(
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
        &model,
        &mut ui,
    );
    assert_eq!(action, ShellAction::Handled);
    assert_eq!(ui.composer.buffer(), "z");
}

#[test]
fn tool_cards_and_verdicts_style_content_deterministically() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c1".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "x"}),
        },
    });
    model.apply(&AgentEvent::ToolResult {
        call_id: "c1".into(),
        output: ToolOutput::Error {
            message: "not found".into(),
        },
        duration_ms: 12,
        speculated: false,
    });
    // A realistic width: the right-hand metric column only lays out when
    // there is room for it, so a 0-width render would drop the duration.
    let lines = transcript_lines(&model, false, 120);
    let joined: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.clone()))
        .collect();
    assert!(joined.contains("read_file"));
    assert!(joined.contains("not found"));
    assert!(joined.contains("12ms"));
    // The tool is named exactly once, by its call row. The result row
    // underneath carries only the failure text and the metric column: the
    // rail already ties it to the call above it, so re-labelling it would
    // spend a second row restating what the first row just said.
    assert_eq!(
        joined.matches("read_file").count(),
        1,
        "the call names the tool; the result does not repeat it: {joined}"
    );
    let line_text =
        |l: &Line<'_>| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() };
    let result = lines
        .iter()
        .find(|l| line_text(l).contains("not found"))
        .expect("the failure renders");
    assert!(
        line_text(result).starts_with(Rail::Fail.prefix()),
        "a failed result rides the ✗ rail: {:?}",
        result.spans
    );
}

/// Expanded (ctrl+o) detail rows — full tool output and pretty-printed
/// call args — align at the subordinate body column ([`BODY`]), exactly
/// where their parent row's content sits, not at the left margin: an
/// expanded body must read as part of the same block rather than as a run
/// of new top-level events.
#[test]
fn expanded_detail_rows_align_at_the_content_column() {
    let indent = " ".repeat(BODY);
    let line_text =
        |l: &Line<'_>| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() };

    let mut result_rows = Vec::new();
    entry_lines(
        &TranscriptEntry::ToolResult {
            call_id: "c1".into(),
            name: "grep".into(),
            ok: true,
            summary: "hit".into(),
            full: "src/a.rs:1: hit\nsrc/b.rs:2: hit".into(),
            duration_ms: 5,
            speculated: false,
            diff: None,
        },
        &[],
        false,
        true,
        false,
        120,
        &mut result_rows,
    );
    // `entry_lines` closes a block with a blank spacer row; the detail rows
    // are everything between the rail row and that trailing gap.
    assert_eq!(
        result_rows.last().map(line_text).as_deref(),
        Some(""),
        "a tool result closes its block with a spacer"
    );
    let details: Vec<String> = result_rows[1..result_rows.len() - 1]
        .iter()
        .map(line_text)
        .collect();
    assert_eq!(details.len(), 2, "both output lines render");
    for d in &details {
        assert!(
            d.starts_with(&indent) && !d.starts_with(&format!("{indent} ")),
            "detail row starts exactly at BODY: {d:?}"
        );
    }

    let mut start_rows = Vec::new();
    entry_lines(
        &TranscriptEntry::ToolStart {
            call_id: "c1".into(),
            name: "grep".into(),
            input: "pattern".into(),
            raw: r#"{"pattern":"hit"}"#.into(),
            path: None,
        },
        &[],
        false,
        true,
        false,
        120,
        &mut start_rows,
    );
    assert!(
        start_rows
            .iter()
            .skip(1)
            .all(|l| line_text(l).starts_with(&indent)),
        "expanded call args align at the content column"
    );
}

// ---- Inline transcript diffs (mutating tool results) ----

/// Additions in the fixture diff below. Sized off [`INLINE_DIFF_CAP`] rather
/// than hardcoded so the collapsed render is always exercising the fold —
/// a fixture that silently slipped under a raised cap would leave the
/// capping contract untested while still passing.
const FIXTURE_ADDS: usize = INLINE_DIFF_CAP + 10;

/// A successful mutation's result entry, plus the file state its
/// [`InlineDiffRef`] resolves against: a one-hunk Rust diff of
/// [`FIXTURE_ADDS`] additions whose freshness seq matches.
fn mutation_entry_and_files() -> (TranscriptEntry, Vec<FileState>) {
    let body: String = (1..=FIXTURE_ADDS)
        .map(|i| format!("+let x{i} = {i};\n"))
        .collect();
    let diff_text = format!("@@ -0,0 +1,{FIXTURE_ADDS} @@\n{body}");
    let entry = TranscriptEntry::ToolResult {
        call_id: "c1".into(),
        name: "edit_file".into(),
        ok: true,
        summary: "ok".into(),
        full: "ok".into(),
        duration_ms: 7,
        speculated: false,
        diff: Some(InlineDiffRef {
            path: "src/x.rs".into(),
            seq: 1,
        }),
    };
    let (added, removed) = crate::diff::count_diff_lines(&diff_text);
    let files = vec![FileState {
        path: "src/x.rs".into(),
        kind: FileChangeKind::Modified,
        latest_diff: Some(diff_text.clone()),
        added,
        removed,
        recent_diffs: [crate::model::RememberedDiff {
            seq: 1,
            text: diff_text,
            added,
            removed,
        }]
        .into_iter()
        .collect(),
        changes: 1,
        reads: 0,
        touched_seq: 1,
    }];
    (entry, files)
}

fn flat_text(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn collapsed_tool_result_shows_a_capped_syntax_highlighted_inline_diff() {
    let (entry, files) = mutation_entry_and_files();
    let mut out = Vec::new();
    entry_lines(&entry, &files, false, false, false, 120, &mut out);
    let text = flat_text(&out);

    // No path rule and no counts footer inline, unlike the standalone diff
    // viewer: the call row above already names the file and the metric
    // column already states `+n −m`, so both rules would be the same facts
    // a second time — four rows of chrome around a small change.
    assert!(
        !text.contains("── src/x.rs"),
        "no path rule inline:\n{text}"
    );
    assert!(
        !text.contains("additions"),
        "no counts footer inline:\n{text}"
    );
    assert!(
        text.contains(&format!("+{FIXTURE_ADDS} −0")),
        "the metric column states the change's size instead:\n{text}"
    );
    // A lone hunk header is dropped too: inline under a call it restates
    // what the line-number gutter beside it already says.
    assert!(
        !text.contains("@@ -0,0"),
        "a single hunk's header is not worth a row inline:\n{text}"
    );
    // The cap counts raw diff lines, hunk header included — so the last
    // addition shown is number INLINE_DIFF_CAP - 1 and the rest fold away.
    let last_shown = INLINE_DIFF_CAP - 1;
    let first_hidden = last_shown + 1;
    let hidden = FIXTURE_ADDS + 1 - INLINE_DIFF_CAP;
    assert!(
        text.contains(&format!("+let x{last_shown} = {last_shown};")),
        "addition {last_shown} is the last one shown:\n{text}"
    );
    assert!(
        !text.contains(&format!("+let x{first_hidden} = {first_hidden};")),
        "addition {first_hidden} is folded behind ctrl+o:\n{text}"
    );
    assert!(
        text.contains(&format!("⋯ {} · ctrl+o", plural_lines(hidden))),
        "the fold names the hidden count and the key:\n{text}"
    );
    // Line-level standard: added lines are numbered on the new side.
    assert!(text.contains("   1 +let x1 = 1;"), "gutter number:\n{text}");
    // Syntax colors ride the path's language (`.rs` → Rust keywords).
    let kw = out
        .iter()
        .flat_map(|l| l.spans.iter())
        .find(|s| s.content == "let")
        .expect("`let` is its own syntax span");
    assert_eq!(kw.style.fg, Some(theme::SYNTAX_KEYWORD));
}

#[test]
fn expanded_tool_result_shows_the_full_inline_diff() {
    let (entry, files) = mutation_entry_and_files();
    let mut out = Vec::new();
    entry_lines(&entry, &files, false, true, false, 120, &mut out);
    let text = flat_text(&out);
    assert!(
        text.contains(&format!("+let x{FIXTURE_ADDS} = {FIXTURE_ADDS};")),
        "ctrl+o reveals every diff line:\n{text}"
    );
    assert!(
        !text.contains("· ctrl+o"),
        "no fold hint once expanded:\n{text}"
    );
    // The size still reads off the metric column, expanded or not — the
    // inline diff never grows a footer rule of its own.
    assert!(
        text.contains(&format!("+{FIXTURE_ADDS} −0")),
        "the metric column still states the change's size:\n{text}"
    );
    assert!(
        !text.contains("additions"),
        "and still without a counts footer:\n{text}"
    );
}

#[test]
fn a_stale_or_unresolvable_diff_ref_renders_no_inline_diff() {
    let (entry, mut files) = mutation_entry_and_files();
    // The path went on mutating until this call's diff aged out of the
    // bounded history — exactly the state `DIFF_HISTORY` evictions leave
    // behind. No remembered diff belongs to this result any more, and
    // showing the newest one instead would attribute a change the call
    // never made, so the row degrades to naming its result.
    files[0].changes = crate::model::DIFF_HISTORY as u32 + 1;
    files[0].recent_diffs = (2..=files[0].changes)
        .map(|seq| crate::model::RememberedDiff {
            seq,
            text: format!("@@ -0,0 +1,1 @@\n+later_edit_{seq}\n"),
            added: 1,
            removed: 0,
        })
        .collect();
    let mut out = Vec::new();
    entry_lines(&entry, &files, false, false, false, 120, &mut out);
    let text = flat_text(&out);
    assert!(
        !text.contains("+let x1 = 1;"),
        "the forgotten diff cannot render:\n{text}"
    );
    assert!(
        !text.contains("later_edit_"),
        "and a newer one is never substituted for it:\n{text}"
    );
    assert!(
        text.contains("ok"),
        "the row falls back to naming the result:\n{text}"
    );

    // A ref whose path is no longer tracked resolves to nothing at all.
    let mut out = Vec::new();
    entry_lines(&entry, &[], false, false, false, 120, &mut out);
    assert!(
        !flat_text(&out).contains("+let x1 = 1;"),
        "unknown path renders no diff"
    );
}

#[test]
fn eviction_marker_renders_as_a_one_line_system_note() {
    let mut out = Vec::new();
    entry_lines(
        &TranscriptEntry::Evicted { count: 1234 },
        &[],
        false,
        false,
        false,
        80,
        &mut out,
    );
    assert_eq!(out.len(), 1, "the marker costs exactly one visual row");
    let text: String = out[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "… 1234 earlier entries evicted");
}

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

// ---- Replay determinism (L-T1) ------------------------------------

/// A small event strategy over a representative spread of variants.
fn any_event() -> impl Strategy<Value = AgentEvent> {
    prop_oneof![
        "[a-z ]{0,12}".prop_map(|delta| AgentEvent::Text { delta }),
        "[a-z ]{0,12}".prop_map(|text| AgentEvent::TextDelta { text }),
        any::<u8>().prop_map(|n| AgentEvent::Stage {
            name: match n % 4 {
                0 => StageKind::Triage,
                1 => StageKind::Plan,
                2 => StageKind::Execute,
                _ => StageKind::Verify,
            },
        }),
        ("[a-z/.]{1,10}", any::<bool>()).prop_map(|(path, created)| AgentEvent::FileChange {
            path,
            kind: if created {
                FileChangeKind::Created
            } else {
                FileChangeKind::Modified
            },
            added: 1,
            removed: 1,
            diff: Some("@@\n-a\n+b".into()),
        }),
        (any::<f64>(), any::<f64>()).prop_map(|(a, b)| AgentEvent::BudgetTick {
            spent_usd: a.abs() % 10.0,
            limit_usd: Some(b.abs() % 10.0),
            mode: BudgetMode::Observed,
            session_spent_usd: None,
            session_limit_usd: None,
        }),
        Just(AgentEvent::Complete {
            model: "glm".into(),
            cost_usd: 0.01,
        }),
    ]
}

proptest! {
    /// The core L-T1 guarantee: folding the same event vector into two
    /// fresh models and rendering both yields byte-identical backing cell
    /// buffers. State derived from the log cannot drift.
    #[test]
    fn replaying_a_log_renders_identical_buffers(events in prop::collection::vec(any_event(), 0..40)) {
        let mut a = UiState::default();
        let mut b = UiState::default();
        let model_a = SessionModel::replay(&events);
        let model_b = SessionModel::replay(&events);

        let mut ta = Terminal::new(TestBackend::new(90, 24)).unwrap();
        let mut tb = Terminal::new(TestBackend::new(90, 24)).unwrap();
        ta.draw(|f| render(&model_a, &mut a, f)).unwrap();
        tb.draw(|f| render(&model_b, &mut b, f)).unwrap();

        prop_assert_eq!(
            buffer_rows(ta.backend().buffer()),
            buffer_rows(tb.backend().buffer())
        );
    }
}
