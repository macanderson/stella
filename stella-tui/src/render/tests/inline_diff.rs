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
