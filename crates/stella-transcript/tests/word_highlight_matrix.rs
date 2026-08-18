// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Word-level highlighting over a fixed matrix, pinned as a committed golden
//! file — and the Rust half of a **cross-language parity check**.
//!
//! [`stella_transcript::word::highlight`] decides which spans of a modified
//! line pair get the stronger tint, and
//! [`stella_transcript::file_diff`]'s change-block pairing decides which
//! removal is compared with which addition. Every Rust surface consumes those
//! two directly. A sixth surface, the arena transcript, reimplements them in
//! TypeScript (`arenabench/ui/lib/word-highlight.ts`) because that page is a
//! Next.js client with no way to call into the workspace — the same situation,
//! and the same remedy, as `stella-diff`'s `view-plan-matrix` (#3558).
//!
//! Before this existed the TypeScript port had **no** word highlighting at all
//! and no mechanism to notice: `transcript-page.tsx` was whole-line tint only,
//! and its own doc comment said the intra-line emphasis was "deliberately not
//! ported". That is the drift #3676 was filed about, and it is the fourth time
//! a hand-port of a Rust renderer has silently fallen behind.
//!
//! The two halves cannot share a test runner, so they share a **file**:
//!
//! - this test regenerates the matrix and asserts it still matches
//!   `tests/fixtures/word-highlight-matrix.txt`, so a Rust change to the rules
//!   cannot land without consciously re-blessing the golden;
//! - `arenabench/ui/scripts/check-word-highlight-parity.mjs` runs the
//!   TypeScript over the same matrix and asserts it reproduces that same file,
//!   so a re-blessed golden fails the TypeScript until it is ported.
//!
//! Re-bless with
//! `BLESS=1 cargo test -p stella-transcript --test word_highlight_matrix`,
//! and then **read the diff**: a golden blessed without looking is a
//! changelog, not a test.

use std::path::PathBuf;

use stella_transcript::word::{self, Span};

/// One `(label, old, new)` pair fed to [`word::highlight`].
type Case = (&'static str, String, String);

/// A token long enough that a run of them clears [`word::LCS_CELL_CAP`].
fn repeat_tokens(n: usize, stem: &str) -> String {
    (0..n)
        .map(|i| format!("{stem}{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The matrix of line pairs.
///
/// Chosen to cover the algorithm's distinct regimes rather than to be large —
/// every branch of [`word::highlight`] that can change what a reader sees:
/// the identity fast path, plain token-level spans, both triggers of the
/// character-level refinement, both `over_cap` bails, and the saturation drop.
/// [`the_matrix_covers_every_regime_the_rules_have`] fails if one stops firing.
fn cases() -> Vec<Case> {
    let mut out: Vec<Case> = [
        // The identity fast path.
        ("identical", "let x = 1;", "let x = 1;"),
        ("both-empty", "", ""),
        // Ordinary token-level edits: one token in many.
        (
            "latex-parindent",
            r"\setlength{\parindent}{15pt}",
            r"\setlength{\parindent}{12pt}",
        ),
        ("one-word-of-many", "the quick brown fox", "the quick lazy fox"),
        ("two-separate-runs", "a b c d e f g", "a X c d e Y g"),
        ("json-value", r#"  "retries": 3,"#, r#"  "retries": 5,"#),
        // Pure insertion and pure deletion — no paired run at all.
        ("insertion-only", "fn main() {}", "pub fn main() {}"),
        ("deletion-only", "pub fn main() {}", "fn main() {}"),
        // The character-level refinement, both triggers.
        ("short-run-length-rule", "value = 1", "value = 12"),
        ("shares-affix-suffix", "--fast", "--fast2"),
        ("shares-affix-prefix", "prefix_old", "prefix_new"),
        ("affix-too-short", "ab", "cd"),
        // Whitespace is its own token, so re-indentation is visible.
        ("reindent", "    body();", "        body();"),
        // Punctuation as single tokens — the brace-run case the module doc names.
        ("brace-run", "{\"a\":{\"b\":1}}", "{\"a\":{\"b\":2}}"),
        // Saturation: too dissimilar to annotate, both sides drop to plain.
        ("wholly-different", "alpha beta gamma", "one two three four"),
        ("empty-against-text", "", "something new"),
        ("text-against-empty", "something old", ""),
        // Multibyte input: Rust counts `char`, the port must not count UTF-16.
        ("cjk", "設定 = 15", "設定 = 12"),
        ("emoji-run", "status: 🟢 ok", "status: 🔴 ok"),
        ("combining", "café = 1", "café = 2"),
    ]
    .into_iter()
    .map(|(label, old, new)| (label, old.to_string(), new.to_string()))
    .collect();

    // `over_cap` on the token-level LCS: 600 tokens a side is 601 * 601 cells
    // over the 250,000 cap, so the pair degrades to plain spans.
    out.push((
        "token-cap-exceeded",
        repeat_tokens(600, "tok"),
        format!("{} tail", repeat_tokens(600, "tok")),
    ));
    // Just *under* the cap on both axes, so the quadratic path really runs:
    // 300 tokens plus separators is ~599 tokens, 600 * 600 = 360,000 — over.
    // 170 stems tokenise to ~339 tokens; 340 * 340 = 115,600, under the cap.
    out.push((
        "token-cap-just-under",
        repeat_tokens(170, "tok"),
        repeat_tokens(170, "tok").replace("tok0 ", "tokZ "),
    ));
    // `refine_short_runs`' own `over_cap` bail: a sole changed run that is the
    // whole line, sharing a long affix, and far too long to re-diff by
    // character. Must keep the token-level spans rather than refining.
    let long = "x".repeat(600);
    out.push((
        "refine-cap-exceeded",
        format!("{long}A"),
        format!("{long}B"),
    ));

    out
}

/// Change blocks fed to the positional pairing in
/// `stella_transcript::file_diff::emit_change_block`, which the port mirrors as
/// `highlightChangeBlock`. The interesting cases are the unbalanced ones: an
/// unpaired line carries no word spans, which is the honest answer because
/// there is nothing to compare it against.
fn blocks() -> Vec<(&'static str, Vec<String>, Vec<String>)> {
    let lines = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    vec![
        ("balanced-1", lines(&["let a = 1;"]), lines(&["let a = 2;"])),
        (
            "balanced-3",
            lines(&["a = 1;", "b = 2;", "c = 3;"]),
            lines(&["a = 9;", "b = 2;", "c = 8;"]),
        ),
        (
            "more-removals",
            lines(&["a = 1;", "b = 2;", "c = 3;"]),
            lines(&["a = 9;"]),
        ),
        (
            "more-additions",
            lines(&["a = 1;"]),
            lines(&["a = 9;", "b = 2;", "c = 3;"]),
        ),
        ("removals-only", lines(&["gone();"]), Vec::new()),
        ("additions-only", Vec::new(), lines(&["added();"])),
    ]
}

/// Escape a span's text for the golden.
///
/// Deliberately hand-rolled rather than `{:?}`: the TypeScript half has to
/// produce the same bytes, and `Debug for str` and `JSON.stringify` disagree
/// on apostrophes, on `\0`, and on how they spell an escaped code point. Five
/// rules, identical in both files, is a contract two languages can keep.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            _ => out.push(ch),
        }
    }
    out
}

/// One side's spans as `plain("…"),changed("…")`.
fn render_spans(spans: &[Span]) -> String {
    spans
        .iter()
        .map(|s| {
            let kind = if s.changed { "changed" } else { "plain" };
            format!("{kind}(\"{}\")", escape(&s.text))
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Render the whole matrix as the text both languages must agree on.
///
/// Deliberately a flat, greppable line per side rather than JSON, for the same
/// reason `view-plan-matrix.txt` is: the file is read by a human deciding
/// whether a re-bless is right. The inputs are **not** echoed — two of them are
/// 3 kB of generated filler — so the case list is mirrored in the TypeScript
/// the way `MATRIX` already is, and `label` is what joins them.
fn render_matrix() -> String {
    let mut out = String::new();
    out.push_str(
        "# stella_transcript::word::highlight over a fixed matrix.\n\
         # Generated — re-bless with:\n\
         #   BLESS=1 cargo test -p stella-transcript --test word_highlight_matrix\n\
         # The TypeScript port must reproduce this byte for byte:\n\
         #   node arenabench/ui/scripts/check-word-highlight-parity.mjs\n\
         # Inputs are not echoed; `cases()`/`blocks()` are mirrored in the port.\n",
    );
    for (label, old, new) in cases() {
        let (old_spans, new_spans) = word::highlight(&old, &new);
        out.push_str(&format!(
            "pair={label} old={}\n",
            render_spans(&old_spans)
        ));
        out.push_str(&format!(
            "pair={label} new={}\n",
            render_spans(&new_spans)
        ));
    }
    for (label, removals, additions) in blocks() {
        let paired = removals.len().min(additions.len());
        let mut removed: Vec<Vec<Span>> = Vec::new();
        let mut added: Vec<Vec<Span>> = Vec::new();
        for k in 0..paired {
            let (o, n) = word::highlight(&removals[k], &additions[k]);
            removed.push(o);
            added.push(n);
        }
        for line in &removals[paired..] {
            removed.push(vec![Span {
                text: line.clone(),
                changed: false,
            }]);
        }
        for line in &additions[paired..] {
            added.push(vec![Span {
                text: line.clone(),
                changed: false,
            }]);
        }
        for (i, spans) in removed.iter().enumerate() {
            out.push_str(&format!(
                "block={label} removed[{i}]={}\n",
                render_spans(spans)
            ));
        }
        for (i, spans) in added.iter().enumerate() {
            out.push_str(&format!(
                "block={label} added[{i}]={}\n",
                render_spans(spans)
            ));
        }
    }
    out
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/word-highlight-matrix.txt")
}

#[test]
fn the_word_rules_still_match_their_committed_matrix() {
    let rendered = render_matrix();
    let path = fixture_path();
    if std::env::var_os("BLESS").is_some() {
        std::fs::create_dir_all(path.parent().expect("fixtures dir")).expect("create fixtures dir");
        std::fs::write(&path, &rendered).expect("write the golden");
        return;
    }
    let golden = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}\n\
             BLESS=1 cargo test -p stella-transcript --test word_highlight_matrix",
            path.display()
        )
    });
    if golden == rendered {
        return;
    }
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
        "word highlighting no longer matches {}.{first}\n\n\
         If the change is intended: re-bless with\n  \
         BLESS=1 cargo test -p stella-transcript --test word_highlight_matrix\n\
         and then port it to arenabench/ui/lib/word-highlight.ts, which is\n\
         checked against this same file by\n  \
         node arenabench/ui/scripts/check-word-highlight-parity.mjs",
        path.display()
    );
}

/// The matrix is only worth pinning if it exercises the rules' distinct
/// outcomes. A matrix that never refines and never saturates would pin a green
/// nothing — which is precisely the failure the arena port was already in.
#[test]
fn the_matrix_covers_every_regime_the_rules_have() {
    let mut identity = false; // old == new, the fast path
    let mut token_spans = false; // ordinary partial highlight
    let mut multi_run = false; // two or more changed runs on a side
    let mut refined = false; // character-level refinement fired
    let mut dropped = false; // saturation or a cap sent it to plain

    for (_, old, new) in cases() {
        let (old_spans, new_spans) = word::highlight(&old, &new);
        let changed_runs = |spans: &[Span]| spans.iter().filter(|s| s.changed).count();
        let plain_whole = |spans: &[Span], line: &str| {
            spans.len() == 1 && !spans[0].changed && spans[0].text == line
        };

        if old == new {
            identity = true;
            continue;
        }
        if plain_whole(&old_spans, &old) && plain_whole(&new_spans, &new) {
            dropped = true;
            continue;
        }
        if changed_runs(&old_spans) > 0 || changed_runs(&new_spans) > 0 {
            token_spans = true;
        }
        if changed_runs(&old_spans) > 1 || changed_runs(&new_spans) > 1 {
            multi_run = true;
        }
        // A refinement is visible as a changed run that is a strict fragment of
        // a token: token granularity alone can only ever tint whole tokens.
        for (spans, line) in [(&old_spans, &old), (&new_spans, &new)] {
            let tokens = word::tokenize(line);
            for span in spans.iter().filter(|s| s.changed) {
                if !tokens.contains(&span.text.as_str()) && !span.text.is_empty() {
                    let whole_tokens = word::tokenize(&span.text)
                        .iter()
                        .all(|t| tokens.contains(t));
                    if !whole_tokens {
                        refined = true;
                    }
                }
            }
        }
    }

    for (covered, name) in [
        (identity, "the identity fast path"),
        (token_spans, "an ordinary partial token highlight"),
        (multi_run, "two or more changed runs on one side"),
        (refined, "the character-level refinement"),
        (dropped, "a drop to plain spans (saturation or a cap)"),
    ] {
        assert!(
            covered,
            "the matrix never exercises: {name} — pinning it proves less than it looks"
        );
    }
}
