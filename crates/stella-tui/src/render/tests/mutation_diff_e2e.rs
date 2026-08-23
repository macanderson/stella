// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **The join nothing covered: fold a real event stream, then render it.**
//!
//! Two suites already sit on either side of this seam and both are green:
//! [`super::super::tests::producer_seq`]'s siblings in `model::tests` prove the
//! fold stamps a resolvable [`InlineDiffRef`], and [`super::inline_diff`] proves
//! the renderer draws a capped, styled diff. Neither proves the pair works
//! *together*, because each supplies by hand what the other would have had to
//! produce — `producer_seq` reads the ref and stops, and `inline_diff` builds
//! its [`FileState`] literally, including a remembered diff starting at `@@`.
//!
//! That gap is not academic. The diff a real turn folds does **not** start at
//! `@@`: `WorkJournal::split_patch_per_file` cuts git's multi-file patch on the
//! `diff --git` header and keeps everything *after* it, so what reaches the fold
//! is `index <a>..<b> <mode>` / `--- a/p` / `+++ b/p` / `@@ …`. Captured from
//! `stella run --output-format stream-json` on 2026-08-21 against `b5f1443a0`:
//!
//! ```text
//! {"type":"file_change","path":"main.rs","kind":"modified","added":1,"removed":1,
//!  "diff":"index a448d2f..993ccff 100644\n--- a/main.rs\n+++ b/main.rs\n@@ -1,3 +1,3 @@\n …"}
//! ```
//!
//! So a header-handling regression in [`crate::diff`] — or any change that
//! breaks the fold's seq derivation — would leave both existing suites green
//! while every `edit_file` row in the product rendered with no diff under it.
//! That is the exact symptom #4155 and #4175 were each reported as, twice, and
//! it is what this file exists to make loud.
//!
//! The event ordering here is the product's own and is load-bearing: the
//! registry measures a solo mutating call the moment it returns and publishes on
//! the channel the engine then sends `ToolResult` on, so `FileChange` folds
//! **before** the row that made it (`stella_tools::call_measure`, #4175).

use super::*;

const WIDTH: usize = 100;

/// Git's real per-file patch shape, header lines included — the bytes
/// `split_patch_per_file` actually hands the fold, not a hand-trimmed hunk.
const REAL_PATCH: &str = "index a448d2f..993ccff 100644\n\
     --- a/main.rs\n\
     +++ b/main.rs\n\
     @@ -1,3 +1,3 @@\n \
     fn main() {\n\
     -    println!(\"alpha\");\n\
     +    println!(\"bravo\");\n \
     }\n";

/// A second and third file's patch, so a multi-path call's changes are
/// distinguishable in the rendered rows by their content and not only by count.
fn patch_for(old: &str, new: &str) -> String {
    format!(
        "index a448d2f..993ccff 100644\n\
         --- a/x\n\
         +++ b/x\n\
         @@ -1,3 +1,3 @@\n \
         fn main() {{\n\
         -    println!(\"{old}\");\n\
         +    println!(\"{new}\");\n \
         }}\n"
    )
}

/// Drive one mutating call through the fold in the order the product emits it.
fn fold_one_call(model: &mut SessionModel, name: &str, path: &str) {
    model.apply(&AgentEvent::ToolStart {
        call: stella_protocol::ToolCall {
            call_id: "c1".into(),
            name: name.into(),
            input: serde_json::json!({ "path": path }),
        },
    });
    // The per-call work-tree measurement, before the result — see the module
    // doc. Reordering these two is itself the #4155 defect.
    model.apply(&AgentEvent::FileChange {
        path: path.into(),
        kind: FileChangeKind::Modified,
        added: 1,
        removed: 1,
        diff: Some(REAL_PATCH.into()),
    });
    model.apply(&AgentEvent::ToolResult {
        call_id: "c1".into(),
        output: stella_protocol::ToolOutput::Ok {
            content: format!("replaced 1 occurrence(s) in {path}"),
            data: None,
        },
        duration_ms: 8,
        speculated: false,
    });
}

/// Drive one `apply_edits` batch that mutates several paths, in the order the
/// product emits it: the call's `ToolStart` naming its first edit's path, one
/// `FileChange` per path from the per-call measurement, then the result.
fn fold_batch(model: &mut SessionModel, paths: &[(&str, &str)]) {
    model.apply(&AgentEvent::ToolStart {
        call: stella_protocol::ToolCall {
            call_id: "c1".into(),
            name: "apply_edits".into(),
            input: serde_json::json!({
                "edits": paths
                    .iter()
                    .map(|(p, _)| serde_json::json!({ "path": p }))
                    .collect::<Vec<_>>(),
            }),
        },
    });
    for (path, marker) in paths {
        model.apply(&AgentEvent::FileChange {
            path: (*path).into(),
            kind: FileChangeKind::Modified,
            added: 1,
            removed: 1,
            diff: Some(patch_for("alpha", marker)),
        });
    }
    model.apply(&AgentEvent::ToolResult {
        call_id: "c1".into(),
        output: stella_protocol::ToolOutput::Ok {
            content: format!("applied {} edits", paths.len()),
            data: None,
        },
        duration_ms: 840,
        speculated: false,
    });
}

/// Render the transcript's last entry against the model's own file state —
/// the same two arguments the deck passes, sourced the same way.
fn render_last(model: &SessionModel) -> String {
    render_last_at(model, false)
}

/// [`render_last`], with the reader's ctrl+o state as the deck passes it.
fn render_last_at(model: &SessionModel, expanded: bool) -> String {
    let idx = model.transcript.len() - 1;
    let mut out = Vec::new();
    entry_lines(
        &model.transcript[idx],
        EntryView::at(&model.files, &model.transcript, idx),
        false,
        expanded,
        false,
        WIDTH,
        &mut out,
    );
    out.iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// **Witness.** A folded `edit_file` call renders the change it made, as a
/// diff, from the bytes the product actually emits.
///
/// Fails on any of the three independent breaks that each produce the same
/// blank row: the fold stamping an unresolvable seq, `FileState` not
/// remembering the patch at it, or [`crate::diff`] mishandling git's `index`/
/// `---`/`+++` preamble.
#[test]
fn a_folded_edit_renders_its_own_diff() {
    let mut model = SessionModel::default();
    fold_one_call(&mut model, "edit_file", "main.rs");
    let text = render_last(&model);

    assert!(
        text.contains("println!(\"bravo\")"),
        "the added line is drawn:\n{text}"
    );
    assert!(
        text.contains("println!(\"alpha\")"),
        "the removed line is drawn:\n{text}"
    );
    assert!(
        text.contains("+1") && text.contains("−1"),
        "the metric column states the measured delta:\n{text}"
    );
}

/// **Witness.** `write_file` is not a second-class citizen of the same path.
///
/// It folds through `is_file_mutation` exactly as `edit_file` does, and the
/// user-visible complaint that started #4155 named both verbs. Pinning only the
/// one that happened to be debugged is how the other regresses unnoticed.
#[test]
fn a_folded_write_renders_its_own_diff() {
    let mut model = SessionModel::default();
    fold_one_call(&mut model, "write_file", "main.rs");
    let text = render_last(&model);

    assert!(
        text.contains("println!(\"bravo\")"),
        "write_file renders a diff too:\n{text}"
    );
}

/// **Witness.** Git's patch preamble is chrome, not content.
///
/// `index a448d2f..993ccff 100644` is a blob-hash line no reader of a
/// transcript wants, and `--- a/main.rs` / `+++ b/main.rs` restate the path the
/// call row above already names. Worse, drawn as body they would be scored as a
/// removal and an addition — `+1 −1` becoming `+2 −2` on a one-line edit.
#[test]
fn the_patch_preamble_is_not_drawn_as_diff_body() {
    let mut model = SessionModel::default();
    fold_one_call(&mut model, "edit_file", "main.rs");
    let text = render_last(&model);

    assert!(
        !text.contains("index a448d2f"),
        "git's blob-hash line is not a transcript row:\n{text}"
    );
    assert!(
        !text.contains("+++ b/main.rs"),
        "the path is named once, by the call row:\n{text}"
    );
}

/// **Witness (#4214).** A call that mutated three paths renders one of them
/// inline and states, honestly, that there are two more.
///
/// Fails on the singular `Option`, where the fold claimed one of the three
/// measurements and the other two expired unclaimed: the row had no count to
/// state and no second file to hide, so both the `3 files` scope and the
/// `⋯ 2 more files` elision row are absent.
///
/// The three assertions are one shape each and are not interchangeable — the
/// count, the summed delta, and the elision row are three different ways for
/// this to go wrong, and #4155/#4156 are precisely a count and a delta that
/// disagreed about their own scope.
#[test]
fn a_three_path_call_renders_one_diff_and_counts_the_rest() {
    let mut model = SessionModel::default();
    fold_batch(
        &mut model,
        &[
            ("crates/a/src/lib.rs", "bravo"),
            ("crates/b/src/lib.rs", "charlie"),
            ("crates/c/src/lib.rs", "delta"),
        ],
    );
    let text = render_last(&model);

    assert!(
        text.contains("println!(\"bravo\")"),
        "the lead change — the batch's first edit — is drawn inline:\n{text}"
    );
    assert!(
        !text.contains("println!(\"charlie\")") && !text.contains("println!(\"delta\")"),
        "the other two are elided, not stacked into the transcript:\n{text}"
    );
    assert!(
        text.contains("3 files"),
        "the row states the scope of its counts:\n{text}"
    );
    assert!(
        text.contains("+3") && text.contains("−3"),
        "the delta is the whole call's, summed over all three paths, not the \
         lead file's +1 −1 standing beside a `3 files` count:\n{text}"
    );
    assert!(
        text.contains("⋯ 2 more files · ctrl+o"),
        "the elision is marked where the missing content was, with the key \
         that reveals it:\n{text}"
    );
}

/// **Witness (#4214).** ctrl+o is not an empty promise: expanding the row draws
/// every file the call moved.
///
/// The collapsed row above advertises two hidden files behind a key. A reveal
/// that showed only the lead file's remaining lines would be the row naming
/// content it cannot produce — the defect class this epic is about, pointed at
/// an affordance instead of a number.
#[test]
fn expanding_a_multi_path_call_reveals_every_file() {
    let mut model = SessionModel::default();
    fold_batch(
        &mut model,
        &[
            ("crates/a/src/lib.rs", "bravo"),
            ("crates/b/src/lib.rs", "charlie"),
            ("crates/c/src/lib.rs", "delta"),
        ],
    );
    let text = render_last_at(&model, true);

    for marker in ["bravo", "charlie", "delta"] {
        assert!(
            text.contains(&format!("println!(\"{marker}\")")),
            "ctrl+o draws {marker}'s file:\n{text}"
        );
    }
    for path in [
        "crates/a/src/lib.rs",
        "crates/b/src/lib.rs",
        "crates/c/src/lib.rs",
    ] {
        assert!(
            text.contains(path),
            "with several diffs stacked, each says which file it is:\n{text}"
        );
    }
}

/// **Witness (#4214), the other direction.** A one-path call renders exactly
/// what it rendered before any of this — byte for byte.
///
/// This is the property the shape was chosen on, and it is pinned rather than
/// assumed. The overwhelmingly common mutation moves one file: if the multi-path
/// rendering leaks into it — a `1 files` chip that never varies, an elision row
/// with nothing elided, a path header over the only diff there is — then the
/// cost of the new case was paid by every ordinary one.
///
/// Deliberately an equality against the whole row list and not a set of
/// `contains`: what is being pinned is that *nothing was added*, and a
/// `contains` cannot see an extra row. Every event here is an `AgentEvent`, so
/// this test compiles and passes unchanged on the code before #4214 — which is
/// what makes it a no-regression pin rather than a description of the new
/// behaviour. Only the right-hand padding is trimmed, since the fixed-width
/// justification is `justify`'s subject and not this test's.
#[test]
fn a_one_path_call_renders_exactly_what_it_did_before() {
    let mut model = SessionModel::default();
    fold_one_call(&mut model, "edit_file", "main.rs");
    let rows: Vec<String> = render_last(&model)
        .lines()
        .map(|l| l.trim_end().to_owned())
        .collect();

    assert_eq!(
        rows,
        vec![
            " │ ⎿                                                                                     \
             +1 −1 · 8ms",
            " │      1  fn main() {",
            " │      2 -    println!(\"alpha\");",
            " │      2 +    println!(\"bravo\");",
            " │      3  }",
        ],
        "a single-path mutation's block is unchanged by #4214"
    );
}
