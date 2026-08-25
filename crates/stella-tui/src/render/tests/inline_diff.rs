//! Inline diffs on a mutating tool result — the capped collapsed view, the
//! full expanded one, and the refs that resolve to nothing.
//!
//! Split out of `render/tests.rs` when that file crossed the 1500-line guard,
//! following `thinking`. One topic per file: the fixture these share is sized
//! off `INLINE_DIFF_CAP` rather than hardcoded, so the collapsed case is always
//! exercising the fold, and nothing outside this cluster uses it — the tests
//! and their fixture move together or not at all.

use super::*;

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
        path: None,
        ok: true,
        summary: "ok".into(),
        full: "ok".into(),
        duration_ms: 7,
        speculated: false,
        diff: vec![InlineDiffRef {
            path: "src/x.rs".into(),
            seq: 1,
        }],
        read_size: None,
        sub_agent_id: None,
    };
    let (added, removed) = crate::diff::count_diff_lines(&diff_text);
    let files = vec![FileState {
        path: "src/x.rs".into(),
        kind: FileChangeKind::Modified,
        added,
        removed,
        recent_diffs: [crate::model::RememberedDiff {
            seq: 1,
            text: Some(diff_text),
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
    entry_lines(
        &entry,
        EntryView::of(&files),
        false,
        false,
        false,
        120,
        &mut out,
    );
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
    // The cap counts raw diff lines, hunk header included, and buys the two
    // ENDS of the change rather than its first `cap` lines. One hunk of
    // `FIXTURE_ADDS` additions cannot fit whole, so the budget splits: the
    // head takes `cap.div_ceil(2)` raw lines (the header plus that many
    // additions less one), the tail takes the remainder off the bottom.
    //
    // This test used to assert the opposite — additions 1 through
    // `cap - 1` and nothing else — which is why the reader could never see
    // where a long edit ended. It is named here as a deliberate change of
    // contract, not a rewritten expectation.
    let head_raw = INLINE_DIFF_CAP.div_ceil(2);
    let tail_raw = INLINE_DIFF_CAP - head_raw;
    let last_head_add = head_raw - 1; // raw line 0 is the hunk header
    let first_tail_add = FIXTURE_ADDS - tail_raw + 1;
    let hidden = FIXTURE_ADDS + 1 - INLINE_DIFF_CAP;
    assert!(
        text.contains(&format!("+let x{last_head_add} = {last_head_add};")),
        "addition {last_head_add} is the last one of the head:\n{text}"
    );
    assert!(
        !text.contains(&format!(
            "+let x{} = {};",
            last_head_add + 1,
            last_head_add + 1
        )),
        "the line after it is inside the elided middle:\n{text}"
    );
    assert!(
        text.contains(&format!("+let x{first_tail_add} = {first_tail_add};")),
        "addition {first_tail_add} opens the tail:\n{text}"
    );
    assert!(
        text.contains(&format!("+let x{FIXTURE_ADDS} = {FIXTURE_ADDS};")),
        "and the change's LAST line is shown — the whole point of a tail:\n{text}"
    );
    assert!(
        text.contains(&format!("⋯ {} · ctrl+o", plural_lines(hidden))),
        "the fold names the hidden count and the key:\n{text}"
    );
    // …and it sits between the two ends, not after them. A marker below the
    // final row would say the change continues past a row that is already
    // the change's last.
    let rows: Vec<&str> = text.lines().collect();
    let fold_at = rows.iter().position(|r| r.contains('⋯')).expect("fold row");
    let last_at = rows
        .iter()
        .position(|r| r.contains(&format!("+let x{FIXTURE_ADDS} = ")))
        .expect("last addition");
    assert!(
        fold_at < last_at,
        "the elision is a middle, not a trailer:\n{text}"
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
    entry_lines(
        &entry,
        EntryView::of(&files),
        false,
        true,
        false,
        120,
        &mut out,
    );
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
    // The path went on mutating and this call's own mutation is no longer
    // remembered — the state a path evicted at `MAX_TRACKED_FILES` and then
    // re-admitted leaves behind. No remembered diff belongs to this result any
    // more, and showing the newest one instead would attribute a change the
    // call never made, so the row degrades to naming its result.
    files[0].changes = 9;
    files[0].recent_diffs = (2..=files[0].changes)
        .map(|seq| crate::model::RememberedDiff {
            seq,
            text: Some(format!("@@ -0,0 +1,1 @@\n+later_edit_{seq}\n")),
            added: 1,
            removed: 0,
        })
        .collect();
    let mut out = Vec::new();
    entry_lines(
        &entry,
        EntryView::of(&files),
        false,
        false,
        false,
        120,
        &mut out,
    );
    let text = flat_text(&out);
    assert!(
        !text.contains("+let x1 = 1;"),
        "the forgotten diff cannot render:\n{text}"
    );
    assert!(
        !text.contains("later_edit_"),
        "and a newer one is never substituted for it:\n{text}"
    );
    // It used to fall back to the tool's own text here. It does not any more:
    // a mutation's one interesting body is its diff, and when that is gone —
    // aged out, superseded, or simply not measured yet — the honest row is
    // quiet. `edit_file` answers "replaced 1 occurrence(s) in <path> at byte
    // 1286 (file sha256/8 e951e674)", which restates the path the head already
    // names and adds an offset and a truncated hash. Rendering it read as
    // though *that* were the report, and made the same edit look informative on
    // one turn and useless on the next with nothing changed but the timing.
    assert!(
        !text.contains("ok"),
        "a mutation with no diff must stay quiet, not print the tool's text:\n{text}"
    );

    // A ref whose path is no longer tracked resolves to nothing at all.
    let mut out = Vec::new();
    entry_lines(
        &entry,
        EntryView::default(),
        false,
        false,
        false,
        120,
        &mut out,
    );
    assert!(
        !flat_text(&out).contains("+let x1 = 1;"),
        "unknown path renders no diff"
    );
}
