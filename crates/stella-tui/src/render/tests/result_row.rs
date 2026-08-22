// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The tool-result row through SPEC 6 (#4127).
//!
//! #4123 routed a call's **head** through the v2 renderer and left the result
//! on v1, because the result carries four things a fresh v2 renderer had not
//! been taught — syntax highlighting in the file's own language, word-level
//! inline diffs, the emitter's line-number gutter, and a truncation notice that
//! has to agree with the export fold. Those four are pinned by
//! [`super::tool_output`] and [`super::inline_diff`], which stayed green
//! through this change and are the evidence that nothing was dropped.
//!
//! What is pinned *here* is the half those files do not watch: that the block
//! is now one event with one rail, and that its body still obeys SPEC 6.4.
//!
//! The head's own `ctrl+o` body — the other thing #4123 unwired — is covered by
//! `block_rail`'s `ctrl_o_on_a_call_reveals_its_arguments` and its two
//! siblings, which landed in #4169. Nothing here re-tests it.

use super::*;
use crate::v2::transcript_source as v2;
use stella_tui_theme::token;

const WIDTH: usize = 100;

fn start(name: &str, path: Option<&str>) -> TranscriptEntry {
    TranscriptEntry::ToolStart {
        call_id: "c1".into(),
        name: name.into(),
        input: path.unwrap_or("x").into(),
        raw: "{\"path\":\"src/lib.rs\"}".into(),
        path: path.map(str::to_owned),
    }
}

fn result(name: &str, ok: bool, path: Option<&str>, body: &str) -> TranscriptEntry {
    TranscriptEntry::ToolResult {
        call_id: "c1".into(),
        name: name.into(),
        path: path.map(str::to_owned),
        ok,
        summary: "ok".into(),
        full: body.into(),
        duration_ms: 42,
        speculated: false,
        diff: None,
    }
}

/// Every rendered row of a block, with the foreground its first span carries —
/// which for a railed row is the rail's own metal.
fn rail_metals(entries: &[TranscriptEntry], expanded: bool) -> Vec<Color> {
    let mut out = Vec::new();
    for entry in entries {
        entry_lines(
            entry,
            EntryView::default(),
            false,
            expanded,
            false,
            WIDTH,
            &mut out,
        );
    }
    out.iter()
        .filter(|l| l.spans.first().is_some_and(|s| s.content.starts_with(" │")))
        .filter_map(|l| l.spans[0].style.fg)
        .collect()
}

/// **The witness.** A mutation's whole block wears one metal.
///
/// It did not. The head rendered through v2 and took its event's gold
/// ([`v2::head_metal`]); the result under it took a fixed muted tone from
/// `Rail::style`, so an `edit_file` block drew `Rgb(239, 197, 63)` on its first
/// row and `Rgb(169, 170, 181)` on every row after it — a rail that changed
/// colour one row into the block it exists to hold together. SPEC 6.2 makes the
/// rail a property of the *event*, and a call and its result are one event.
///
/// Asserted as "one distinct metal, and it is the call's" rather than against a
/// literal: a regression that repainted both halves the same *wrong* colour
/// would satisfy the first half alone.
#[test]
fn a_mutations_whole_block_wears_one_metal() {
    let metals = rail_metals(
        &[
            start("edit_file", Some("src/lib.rs")),
            result(
                "edit_file",
                true,
                Some("src/lib.rs"),
                "applied\nline two\nline three",
            ),
        ],
        // Expanded, because a collapsed mutation with no diff now draws no
        // body at all — and a block with no body rows cannot demonstrate that
        // its rows share a metal. The invariant under test is unchanged; the
        // fixture just has to be a block.
        true,
    );
    assert!(metals.len() >= 4, "the block lost rows: {metals:?}");
    let distinct: std::collections::BTreeSet<String> =
        metals.iter().map(|c| format!("{c:?}")).collect();
    assert_eq!(
        distinct.len(),
        1,
        "the block's rail changes metal partway down: {distinct:?}"
    );
    assert_eq!(
        metals[0],
        v2::head_metal("edit_file"),
        "and the metal is the call's own (SPEC 6.2), not a tone picked by the \
         row that happens to be drawing"
    );
}

/// The same, for the other metal — a read is the world coming in, so the whole
/// block is silver-dim rather than gold.
///
/// Kept apart from the mutation case because a renderer that hardcoded gold for
/// every block would pass that one: the point of SPEC 6.2's table is that the
/// margin answers *what kind of thing this was* before anything is read.
#[test]
fn a_reads_whole_block_wears_the_reading_metal() {
    let metals = rail_metals(
        &[
            start("read_file", Some("src/main.rs")),
            result(
                "read_file",
                true,
                Some("src/main.rs"),
                "     1\tfn main() {}",
            ),
        ],
        false,
    );
    let distinct: std::collections::BTreeSet<String> =
        metals.iter().map(|c| format!("{c:?}")).collect();
    assert_eq!(
        distinct.len(),
        1,
        "a read's block is not one metal: {distinct:?}"
    );
    assert_eq!(metals[0], token::MUTED, "a read takes the silver-dim rail");
    assert_ne!(
        v2::head_metal("read_file"),
        v2::head_metal("edit_file"),
        "the two metals must differ, or the rail says nothing"
    );
}

/// A failure overrides its call's metal on every row of the result — and keeps
/// a glyph, because SPEC 13 will not let colour be the only carrier.
#[test]
fn a_failure_overrides_the_metal_and_still_says_so_without_colour() {
    let mut out = Vec::new();
    entry_lines(
        &result(
            "bash",
            false,
            None,
            "error[E0432]: unresolved import\n  --> src/lib.rs:3:5",
        ),
        EntryView::default(),
        false,
        false,
        false,
        WIDTH,
        &mut out,
    );
    let railed: Vec<_> = out
        .iter()
        .filter(|l| l.spans.first().is_some_and(|s| s.content.starts_with(" │")))
        .collect();
    assert!(!railed.is_empty(), "the failed result lost its rail");
    for line in &railed {
        assert_eq!(
            line.spans[0].style.fg,
            Some(theme::DANGER),
            "a failed result row is not in the danger metal"
        );
    }
    let text: String = railed[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        text.contains('✗'),
        "a failure must be legible with the colour switched off: {text:?}"
    );
}

// ── SPEC 6.4: the diff body ─────────────────────────────────────────────────

/// A Rust diff renders add and remove rows with everything SPEC 6.4 names: a
/// dim line-number gutter, a **coloured sign column** (never colour alone), the
/// language's own syntax foreground, and an add/remove ground.
///
/// The rows themselves are `crate::diff`'s and are tested there; what this pins
/// is that routing the result through v2 did not cost them — which is the
/// specific loss #4123 refused to take.
#[test]
fn a_rust_diff_renders_add_and_remove_rows_per_spec_64() {
    let diff_text = "@@ -1,2 +1,2 @@\n-let old = 1;\n+let new = 2;\n";
    let (added, removed) = crate::diff::count_diff_lines(diff_text);
    let files = vec![FileState {
        path: "src/x.rs".into(),
        kind: FileChangeKind::Modified,
        latest_diff: Some(diff_text.to_string()),
        added,
        removed,
        recent_diffs: [crate::model::RememberedDiff {
            seq: 1,
            text: Some(diff_text.to_string()),
            added,
            removed,
        }]
        .into_iter()
        .collect(),
        changes: 1,
        reads: 0,
        touched_seq: 1,
    }];
    let entry = TranscriptEntry::ToolResult {
        call_id: "c1".into(),
        name: "edit_file".into(),
        path: None,
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
    let mut out = Vec::new();
    entry_lines(
        &entry,
        EntryView::of(&files),
        false,
        false,
        false,
        WIDTH,
        &mut out,
    );

    let ground = |bg: Color| -> Vec<&Line<'static>> {
        out.iter()
            .filter(|l| l.spans.iter().any(|s| s.style.bg == Some(bg)))
            .collect()
    };
    let adds = ground(theme::DIFF_ADD_BG);
    let dels = ground(theme::DIFF_DEL_BG);
    assert_eq!(adds.len(), 1, "one added row");
    assert_eq!(dels.len(), 1, "one removed row");

    for (rows, sign, fg) in [(&adds, '+', theme::OK), (&dels, '-', theme::BAD)] {
        let row = rows[0];
        // The sign column is mandatory and coloured — colour is never the only
        // diff signal, so the glyph has to be there when the ground is not
        // rendered at all (a 16-colour terminal, a piped capture).
        let marker = row
            .spans
            .iter()
            .find(|s| s.content.trim() == sign.to_string())
            .unwrap_or_else(|| panic!("no `{sign}` sign column on the row"));
        assert_eq!(marker.style.fg, Some(fg), "the `{sign}` sign is uncoloured");
        // The gutter is chrome and stays dim. Found by content rather than by
        // index: the rail's margin is more than one span, and an index here
        // would be asserting about whichever cell the margin happens to end on.
        let gutter = row
            .spans
            .iter()
            .find(|s| s.content.trim() == "1")
            .unwrap_or_else(|| panic!("no line-number gutter on the `{sign}` row"));
        assert_eq!(
            gutter.style.fg,
            Some(theme::MUTED),
            "the line-number gutter is not in the dim tone"
        );
        // Syntax foreground survives on top of the ground: two layers, per 6.4.
        assert!(
            row.spans
                .iter()
                .any(|s| s.content == "let" && s.style.fg == Some(theme::SYNTAX_KEYWORD)),
            "the diff row lost its syntax colouring"
        );
    }

    // And the ground is a *band*: it runs to the pane edge rather than tracing
    // the ragged end of the code. SPEC 6.4 phrases this as `Line.style` holding
    // the tint; the deck pads a trailing span instead, because a line-level
    // style would also paint the rail — which is the call's metal, not the
    // diff's. Same two layers, margin excluded.
    for (rows, bg) in [(&adds, theme::DIFF_ADD_BG), (&dels, theme::DIFF_DEL_BG)] {
        assert_eq!(
            rows[0].width(),
            WIDTH,
            "a tinted diff row stops short of the pane edge"
        );
        let last = rows[0].spans.last().expect("a trailing span");
        assert_eq!(last.style.bg, Some(bg), "the band does not reach the edge");
    }

    // The rail is emphatically NOT tinted — it belongs to the event, not to the
    // hunk under it.
    for row in adds.iter().chain(dels.iter()) {
        assert_eq!(
            row.spans[0].style.bg, None,
            "the diff's ground bled onto the block's rail"
        );
    }
}

/// **The witness for #4155's render half.** A change the emitter *measured* but
/// could not attach a patch for states its size, and invents no diff.
///
/// The two facts arrive independently — a producer builds each change from a
/// name-status listing and then attaches numstat and diff text in two separate
/// calls, either of which can fail — so [`FileState::delta_at`] can answer
/// `Some((added, removed))` on a `seq` whose [`FileState::diff_at`] answers
/// `None`. The fixture below is exactly that shape, and the assertion at the
/// top of the test pins it: if the two ever resolved together, this test would
/// be watching the ordinary case and proving nothing.
///
/// The metric column therefore gates on the *measurement*, not on the text
/// (`render/entry.rs`, `if let Some((added, removed)) = inline_delta`). Both
/// halves of that expression are asserted here, because the two known ways to
/// get it wrong fail different halves:
///
/// * The pre-#4155 form — `if inline.is_some()` with `unwrap_or((0, 0))` —
///   fails the first half: `inline` is `None` here, so the row rendered no size
///   at all despite knowing one. (Its other defect, a fabricated `+0 −0` over a
///   real edit, is the one #4156 removed from the head row.)
/// * A "fix" that reached for the patch it does not have — falling back to the
///   file's `latest_diff`, or recounting a rendered hunk — fails the second: no
///   diff ground may be painted for a mutation with no diff.
///
/// The third assertion is the body. A successful file tool's own prose is never
/// a transcript body (`body_is_tool_prose` in `render/entry.rs`): an `edit_file`
/// returns a sentence restating the path with a byte offset and a truncated
/// hash, and a row that falls back to it reads as though *that* were the report.
/// So the honest row here is the head, its size, and nothing else.
///
/// Worth pinning rather than shrugging at: the expression already survived one
/// silent near-revert, when #4198 moved the tool block to a new path carrying a
/// pre-fix copy. Git reported no conflict, because the two versions were never
/// at the same path — it was caught by hand-diffing the decision points.
#[test]
fn a_measured_change_with_no_patch_states_its_size_and_invents_no_diff() {
    let files = vec![FileState {
        path: "src/x.rs".into(),
        kind: FileChangeKind::Modified,
        // No patch anywhere on the path — not on the mutation, not as a
        // fallback — so nothing the row could reach for would produce one.
        latest_diff: None,
        added: 3,
        removed: 1,
        recent_diffs: [crate::model::RememberedDiff {
            seq: 1,
            text: None,
            added: 3,
            removed: 1,
        }]
        .into_iter()
        .collect(),
        changes: 1,
        reads: 0,
        touched_seq: 1,
    }];
    assert_eq!(
        (files[0].diff_at(1), files[0].delta_at(1)),
        (None, Some((3, 1))),
        "the fixture is not a measured-but-patchless change, so the row's two \
         gates would resolve together and the test would prove nothing"
    );

    let entry = TranscriptEntry::ToolResult {
        call_id: "c1".into(),
        name: "edit_file".into(),
        path: None,
        ok: true,
        summary: "ok".into(),
        full: "replaced 1 occurrence(s) in src/x.rs at byte 1286".into(),
        duration_ms: 7,
        speculated: false,
        diff: Some(InlineDiffRef {
            path: "src/x.rs".into(),
            seq: 1,
        }),
    };
    let mut out = Vec::new();
    entry_lines(
        &entry,
        EntryView::of(&files),
        false,
        false,
        false,
        WIDTH,
        &mut out,
    );

    // 1. The size the emitter measured, in the metric column's own tones.
    let styled = |text: &str, fg: Color| -> bool {
        out.iter().any(|l| {
            l.spans
                .iter()
                .any(|s| s.content.trim() == text && s.style.fg == Some(fg))
        })
    };
    let rendered: Vec<String> = out
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    assert!(
        styled("+3", theme::OK),
        "the row knows the change added 3 lines and does not say so: {rendered:?}"
    );
    assert!(
        styled("−1", theme::BAD),
        "the row knows the change removed 1 line and does not say so: {rendered:?}"
    );

    // 2. And it invents no patch to go with them.
    let ground = |bg: Color| -> usize {
        out.iter()
            .filter(|l| l.spans.iter().any(|s| s.style.bg == Some(bg)))
            .count()
    };
    assert_eq!(
        (ground(theme::DIFF_ADD_BG), ground(theme::DIFF_DEL_BG)),
        (0, 0),
        "a mutation with no patch rendered diff rows anyway: {rendered:?}"
    );

    // 3. Nor does it fall back to the tool's sentence about itself.
    assert!(
        !rendered.iter().any(|l| l.contains("byte 1286")),
        "the row fell back to the tool's own prose: {rendered:?}"
    );
}
