//! How a chain of thought renders — the collapsed window, the live tail, and
//! the expanded whole.
//!
//! Split out of `render/tests.rs` when that file crossed the 1500-line guard.
//! One topic per file: these are the only tests that care what a
//! `TranscriptEntry::Reasoning` looks like, and between them they pin the
//! contract in both directions — a thought still being written follows its
//! tail so the newest text is always the bottom row, and a settled one reverts
//! to its head, because scrollback is read top-down.

use super::*;

#[test]
fn thinking_is_collapsed_by_default_and_expands_with_the_toggle() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::Reasoning {
        delta: "alpha\nbeta\ngamma".into(),
    });
    let mut ui = UiState::default();
    let collapsed = draw(&model, &mut ui, 100, 24);
    // A system note is its own rail: the `⏵ thinking` label already opens
    // with a glyph in column 0, so it indexes the margin without a second
    // marker (and without the old right-aligned `[…]: ` tag column).
    assert!(
        collapsed.contains("⏵ thinking  3 lines"),
        "collapsed header:\n{collapsed}"
    );
    // Collapsed = header + 3 preview lines (all 3 fit within preview count),
    // then the spacer every block-closing entry ends on.
    let c_lines = transcript_lines(&model, false, 0);
    assert_eq!(c_lines.len(), 5, "header + 3 preview lines + block spacer");
    // Preview shows the reasoning content.
    let c_text: String = c_lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    assert!(
        c_text.contains("alpha"),
        "collapsed shows first line:\n{c_text}"
    );

    ui.thinking_expanded = true;
    let expanded = draw(&model, &mut ui, 100, 24);
    assert!(
        expanded.contains("alpha") && expanded.contains("gamma"),
        "expanded shows the full reasoning:\n{expanded}"
    );
    // Expanded = header + 3 content lines + the same block spacer.
    assert_eq!(transcript_lines(&model, true, 0).len(), 5);
}

#[test]
fn collapsed_thinking_shows_preview_lines_not_the_full_wall() {
    let mut model = SessionModel::new();
    let long = format!("{}THE-TAIL", "reasoning noise ".repeat(20));
    model.apply(&AgentEvent::Reasoning { delta: long });
    let lines = transcript_lines(&model, false, 0);
    // 1 line of text → header + 1 preview, then the block spacer.
    assert_eq!(lines.len(), 3);
    // The preview should be visible.
    let text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    assert!(text.contains("reasoning"), "preview shows content:\n{text}");
}

#[test]
fn collapsed_thinking_preview_updates_as_deltas_stream() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::Reasoning {
        delta: "planning the refactor".into(),
    });
    let before = transcript_lines(&model, false, 0);
    model.apply(&AgentEvent::Reasoning {
        delta: " now checking tests".into(),
    });
    let after = transcript_lines(&model, false, 0);
    // Both produce header + 1 preview + the block spacer.
    assert_eq!(before.len(), 3);
    assert_eq!(after.len(), 3, "still header + 1 preview line");
    // The preview line (index 1) visibly changes with each delta.
    assert_ne!(
        before[1], after[1],
        "the preview visibly changes with each delta"
    );
}

/// Flatten transcript rows to one trimmed string per row. Trimmed because
/// these tests assert *which* rows a thought shows and in what order — the
/// body indent is `push_note_block`'s job and has its own coverage.
fn row_texts(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .trim()
                .to_string()
        })
        .collect()
}

/// The rows up to the block spacer that closes the first entry.
fn first_block(rows: &[String]) -> &[String] {
    let end = rows.iter().position(|r| r.is_empty()).unwrap_or(rows.len());
    &rows[..end]
}

/// A thought long enough to overflow the collapsed row budget, one logical
/// line per numbered step.
fn numbered_thought(steps: usize) -> String {
    (1..=steps)
        .map(|i| format!("step-{i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The whole point of the change: on a long turn the agent sits inside one
/// thought for minutes, and a preview pinned to its opening lines is
/// indistinguishable from a hung session. While the thought is live the block
/// follows its tail, so the bottom row is always the newest text.
#[test]
fn a_live_thought_follows_its_tail_and_ends_on_the_newest_row() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::Reasoning {
        delta: numbered_thought(9),
    });
    assert!(
        reasoning_is_live(&model.transcript, &model.streaming_text),
        "nothing has landed after it"
    );
    let rows = row_texts(&transcript_lines(&model, false, 0));
    // header + fold hint + 5 tail rows + block spacer.
    assert_eq!(rows.len(), 8, "rows:\n{rows:#?}");
    assert!(rows[0].contains("9 lines"), "header counts them all");
    assert!(rows[1].contains("⋯"), "the fold marker sits *above* a tail");
    let window: Vec<&str> = rows[2..7].iter().map(String::as_str).collect();
    assert_eq!(
        window,
        ["step-5", "step-6", "step-7", "step-8", "step-9"],
        "the last five steps, in order — and the head has scrolled off"
    );
}

/// Streaming the next step scrolls the window by exactly one row — the tail
/// keeps moving rather than the block keeping growing.
#[test]
fn the_live_tail_scrolls_as_each_new_line_arrives() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::Reasoning {
        delta: numbered_thought(9),
    });
    let before = row_texts(&transcript_lines(&model, false, 0));
    model.apply(&AgentEvent::Reasoning {
        delta: "\nstep-10".into(),
    });
    let after = row_texts(&transcript_lines(&model, false, 0));
    assert_eq!(before.len(), after.len(), "same height");
    assert_eq!(
        after[after.len() - 2],
        "step-10",
        "the newest line is the last row:\n{after:#?}"
    );
    assert_eq!(&after[2..6], &before[3..7], "the window slid by one row");
}

/// Once anything lands after the thought it is settled scrollback, and
/// scrollback is read top-down: back to the head preview, fold marker below it.
#[test]
fn a_settled_thought_reverts_to_its_head() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::Reasoning {
        delta: numbered_thought(9),
    });
    model.apply(&AgentEvent::Text {
        delta: "here is the answer".into(),
    });
    assert!(
        !reasoning_is_live(&model.transcript, &model.streaming_text),
        "the answer displaced it"
    );
    let rows = row_texts(&transcript_lines(&model, false, 0));
    assert_eq!(rows[1], "step-1", "the head is back:\n{rows:#?}");
    assert_eq!(rows[5], "step-5");
    assert!(
        rows[6].contains("⋯"),
        "and the fold marker moved below it:\n{rows:#?}"
    );
}

/// A turn that ends *on* a thought — no answer text, no tool call — settles
/// too, and this is what lets liveness stay purely positional: every terminal
/// event appends an entry of its own, so "is anything after it" already answers
/// "is the turn over". If `Complete` or `Error` ever stopped landing an entry,
/// a finished thought would tail-follow forever — hence the assertion on both.
#[test]
fn a_terminal_event_settles_a_trailing_thought() {
    for terminal in [
        AgentEvent::Complete {
            model: "glm-5.2".into(),
            cost_usd: 0.01,
        },
        AgentEvent::Error {
            message: "upstream 500".into(),
            retryable: false,
        },
    ] {
        let mut model = SessionModel::new();
        model.apply(&AgentEvent::Reasoning {
            delta: numbered_thought(9),
        });
        assert!(reasoning_is_live(&model.transcript, &model.streaming_text));
        model.apply(&terminal);
        assert!(
            !reasoning_is_live(&model.transcript, &model.streaming_text),
            "{terminal:?} left the thought looking live"
        );
    }
}

/// The budget is spent in *visual rows*, not logical lines. A chain of thought
/// is usually a few paragraph-long lines, so a three-*line* window on a
/// wrapped thought is most of the pane — and the block has to be the same
/// height before and after the stream stops or it resizes under the reader.
#[test]
fn the_collapsed_row_budget_survives_a_single_paragraph_thought() {
    let mut model = SessionModel::new();
    // One logical line, far wider than the pane.
    let para = format!("{}THE-TAIL", "reasoning noise ".repeat(40));
    model.apply(&AgentEvent::Reasoning { delta: para });
    let live = row_texts(&transcript_lines(&model, false, 60));
    let live_block = first_block(&live).to_vec();
    assert_eq!(live_block.len(), 7, "header + hint + 5 rows:\n{live:#?}");
    assert!(
        live_block[live_block.len() - 1].contains("THE-TAIL"),
        "the newest text is on screen, not 600 chars above it:\n{live:#?}"
    );
    // Settling must not change the height.
    model.apply(&AgentEvent::Text {
        delta: "done".into(),
    });
    let settled = row_texts(&transcript_lines(&model, false, 60));
    let settled_block = first_block(&settled);
    assert_eq!(
        settled_block.len(),
        live_block.len(),
        "same block height once settled:\n{settled:#?}"
    );
    assert!(
        !settled_block.iter().any(|r| r.contains("THE-TAIL")),
        "showing the head now:\n{settled_block:#?}"
    );
}

/// A short thought needs no window at all — no fold marker, and identical
/// live or settled.
#[test]
fn a_thought_inside_the_budget_renders_whole_either_way() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::Reasoning {
        delta: "alpha\nbeta".into(),
    });
    let live = row_texts(&transcript_lines(&model, false, 0));
    assert_eq!(live, vec!["⏵ thinking  2 lines", "alpha", "beta", ""]);
    model.apply(&AgentEvent::Text { delta: "x".into() });
    let settled = row_texts(&transcript_lines(&model, false, 0));
    assert_eq!(&settled[..4], &live[..4], "no marker, no reflow");
}

/// The expanded view is unconditional: ctrl+r shows the whole thought whether
/// it is live or settled, blank lines and all.
#[test]
fn expanding_a_live_thought_still_shows_all_of_it() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::Reasoning {
        delta: numbered_thought(9),
    });
    let rows = row_texts(&transcript_lines(&model, true, 0));
    assert!(rows[0].contains("⏶"), "chevron flips:\n{rows:#?}");
    assert_eq!(rows[1], "step-1");
    assert_eq!(rows[9], "step-9");
    assert!(
        !rows.iter().any(|r| r.contains('⋯')),
        "nothing is folded:\n{rows:#?}"
    );
}
