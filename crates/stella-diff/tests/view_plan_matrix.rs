// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The view policy's answer over a fixed matrix, pinned as a committed golden
//! file — and the Rust half of a **cross-language parity check**.
//!
//! [`stella_diff::view::plan`] decides how much of a diff every Stella surface
//! draws and which part. Four surfaces consume it in Rust; a fifth, the arena
//! transcript, reimplements it in TypeScript
//! (`arenabench/ui/lib/diff-view.ts::selectDiffLines`) because that page is a
//! Next.js client with no way to call into the workspace. Two implementations
//! of one policy, and an arena transcript is read *beside* the deck and the
//! Observatory when someone is arguing about a run — so drift there means one
//! edit looking like two.
//!
//! The two halves cannot share a test runner, so they share a **file**:
//!
//! - this test regenerates the matrix and asserts it still matches
//!   `tests/fixtures/view-plan-matrix.txt`, so a Rust change to the policy
//!   cannot land without consciously re-blessing the golden;
//! - `arenabench/ui/scripts/check-diff-view-parity.mjs` runs the TypeScript
//!   over the same matrix and asserts it reproduces that same file, so a
//!   re-blessed golden fails the TypeScript until it is ported.
//!
//! Re-bless with `BLESS=1 cargo test -p stella-diff --test view_plan_matrix`,
//! and then **read the diff**: a golden blessed without looking is a
//! changelog, not a test.
//!
//! This replaces a manual ritual — a `plan_matrix` example a human was
//! expected to remember to run — that was strictly weaker than a gate, and
//! documented as such (#3558). Its one run before being automated is why it
//! earned the upgrade: the first diff disagreed, and the bug was in the Rust
//! (`Plan::fold_before` drew the marker *after* the only surviving hunk
//! whenever the budget kept a tail and no head). Thirty-eight hand-written
//! unit tests passed over it, because every one of them happened to keep a
//! head.

use std::path::PathBuf;

/// The matrix: `(total lines, lines per hunk)` pairs, each swept over every
/// budget from 0 to [`CAP_MAX`].
///
/// Chosen to cover the policy's distinct regimes rather than to be large.
/// `(15, 3)` and `(80, 4)` are ordinary multi-hunk diffs; `(31, 31)` and
/// `(500, 500)` are the single-enormous-hunk shape a created file has, which
/// is the only path into the line-window fallback; `(6, 3)` is small enough
/// that a budget straddles "everything fits" and "one hunk fits"; and
/// `(100, 7)` is the one that does **not** divide evenly, leaving a short
/// final hunk — which is what produces the keep-only-a-tail case that the
/// original `fold_before` got wrong.
const MATRIX: &[(usize, usize)] = &[(15, 3), (80, 4), (31, 31), (100, 7), (6, 3), (500, 500)];

/// Budgets swept per matrix entry. Past `INLINE_CAP` by enough to cover the
/// "everything fits" boundary for the small entries.
const CAP_MAX: usize = 40;

/// Render the whole matrix as the text both languages must agree on.
///
/// Deliberately a flat, greppable line per case rather than JSON: the file is
/// read by a human deciding whether a re-bless is right, and a diff of
/// `shown=[0-6,12-15]` says what changed at a glance.
fn render_matrix() -> String {
    let mut out = String::new();
    out.push_str(
        "# stella_diff::view::plan over a fixed matrix.\n\
         # Generated — re-bless with:\n\
         #   BLESS=1 cargo test -p stella-diff --test view_plan_matrix\n\
         # The TypeScript port must reproduce this byte for byte:\n\
         #   node arenabench/ui/scripts/check-diff-view-parity.mjs\n\
         # `fold` is the index the elision marker precedes, or `-` for none.\n",
    );
    for &(total, step) in MATRIX {
        // One `@@` every `step` lines: the hunk starts the policy is given.
        let starts: Vec<usize> = (0..total).step_by(step).collect();
        for cap in 0..=CAP_MAX {
            let plan = stella_diff::view::plan(&starts, total, cap);
            let shown: Vec<String> = plan
                .shown
                .iter()
                .map(|r| format!("{}-{}", r.start, r.end))
                .collect();
            let fold = match plan.fold_before() {
                Some(i) => i.to_string(),
                None => "-".to_string(),
            };
            out.push_str(&format!(
                "total={total} step={step} cap={cap} shown=[{}] hidden={} fold={fold}\n",
                shown.join(","),
                plan.hidden,
            ));
        }
    }
    out
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/view-plan-matrix.txt")
}

#[test]
fn the_view_policy_still_matches_its_committed_matrix() {
    let rendered = render_matrix();
    let path = fixture_path();
    if std::env::var_os("BLESS").is_some() {
        std::fs::create_dir_all(path.parent().expect("fixtures dir")).expect("create fixtures dir");
        std::fs::write(&path, &rendered).expect("write the golden");
        return;
    }
    let golden = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}\nBLESS=1 cargo test -p stella-diff --test view_plan_matrix",
            path.display()
        )
    });
    if golden == rendered {
        return;
    }
    // Name the first differing line rather than dumping two 250-line blobs:
    // the answer to "what did I change about the policy" is one row.
    let first = golden
        .lines()
        .zip(rendered.lines())
        .find(|(a, b)| a != b)
        .map(|(a, b)| format!("\n  golden: {a}\n  now:    {b}"))
        .unwrap_or_else(|| {
            format!(
                "\n  (no differing line — the length changed: {} golden vs {} now)",
                golden.lines().count(),
                rendered.lines().count()
            )
        });
    panic!(
        "the view policy no longer matches {}.{first}\n\n\
         If the change is intended: re-bless with\n  \
         BLESS=1 cargo test -p stella-diff --test view_plan_matrix\n\
         and then port it to arenabench/ui/lib/diff-view.ts, which is checked\n\
         against this same file by\n  \
         node arenabench/ui/scripts/check-diff-view-parity.mjs",
        path.display()
    );
}

/// The matrix is only worth pinning if it actually exercises the policy's
/// distinct outcomes. A matrix that never elides, or never reaches the
/// line-window fallback, would pin a green nothing.
#[test]
fn the_matrix_covers_every_regime_the_policy_has() {
    let mut whole = false; // everything fits
    let mut head_only = false; // whole hunks from the front, nothing from the back
    let mut head_and_tail = false; // both ends, elided middle
    let mut tail_only = false; // the keep-only-a-tail case
    let mut window = false; // no whole hunk fits
    let mut empty = false; // zero budget

    for &(total, step) in MATRIX {
        let starts: Vec<usize> = (0..total).step_by(step).collect();
        for cap in 0..=CAP_MAX {
            let plan = stella_diff::view::plan(&starts, total, cap);
            match (plan.shown.len(), plan.hidden) {
                (_, 0) => whole = true,
                (0, _) => empty = true,
                (1, _) if plan.shown[0].start == 0 => {
                    // A single leading range is head-only — unless the whole
                    // diff is one hunk, in which case it is the window
                    // fallback that happened to have no room for a tail.
                    if starts.len() == 1 && cap == 1 {
                        window = true;
                    } else {
                        head_only = true;
                    }
                }
                (1, _) => tail_only = true,
                (2, _) => {
                    if starts.len() == 1 {
                        window = true;
                    } else {
                        head_and_tail = true;
                    }
                }
                _ => panic!("more than two ranges: {plan:?}"),
            }
        }
    }

    for (covered, name) in [
        (whole, "everything fits"),
        (head_only, "whole hunks from the front only"),
        (head_and_tail, "both ends with an elided middle"),
        (tail_only, "only a tail survives"),
        (window, "no whole hunk fits — the line-window fallback"),
        (empty, "a zero budget"),
    ] {
        assert!(
            covered,
            "the matrix never exercises: {name} — pinning it proves less than it looks"
        );
    }
}
