// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Word-level highlighting over a fixed matrix of line pairs, pinned as a
//! committed golden — and the Rust half of a **cross-language parity check**.
//!
//! [`stella_transcript::word::highlight`] decides what inside a modified line
//! is tinted, for the Command Deck's character grid and for the jet-black web
//! transcript the Observatory serves. The arena's transcript page reimplements
//! it in TypeScript (`arenabench/ui/lib/word-diff.ts::highlight`) because that
//! page is a Next.js client with no way to call into the workspace. Two
//! implementations of one policy — and an arena transcript is read *beside* the
//! deck and the Observatory when someone is arguing about what a run did, which
//! is exactly when one edit must not look like two.
//!
//! The two halves cannot share a test runner, so they share a **file**, the
//! arrangement `stella-diff`'s `view_plan_matrix` established:
//!
//! - this test regenerates the matrix and asserts it still matches
//!   `tests/fixtures/word-highlight-matrix.txt`, so a Rust change to the policy
//!   cannot land without consciously re-blessing the golden;
//! - `arenabench/ui/scripts/check-word-diff-parity.mjs` runs the TypeScript
//!   over the same matrix and asserts it reproduces that same file, so a
//!   re-blessed golden fails the TypeScript until it is ported.
//!
//! Re-bless with
//! `BLESS=1 cargo test -p stella-transcript --test word_highlight_matrix`, and
//! then **read the diff**: a golden blessed without looking is a changelog, not
//! a test.

use std::path::PathBuf;

/// The matrix: `(old, new)` line pairs, chosen to cover the policy's distinct
/// regimes rather than to be large.
///
/// Each comment names the regime it is the witness for. A pair that exercises
/// nothing the others do not is not worth a golden row, because every row here
/// is a line a human has to read when deciding whether a re-bless is right.
const MATRIX: &[(&str, &str)] = &[
    // Identical: the early return, before any tokenising.
    ("same line", "same line"),
    // The motivating case — one changed token in a line of punctuation.
    (
        r"\setlength{\parindent}{15pt}",
        r"\setlength{\parindent}{12pt}",
    ),
    // Punctuation must not lump: a tokeniser joining `}{` reports the change
    // as spanning the brace run.
    ("{a}{b}", "{a}{c}"),
    // Character-level fallback via the length rule: one token, both sides
    // shorter than SHORT_RUN.
    ("x = 1", "x = 2"),
    // Character-level fallback via the affix rule: both tokens are longer than
    // SHORT_RUN, and the honest answer is that one character was appended.
    ("--fast", "--fast2"),
    // A shared prefix long enough to trip the affix rule from the front.
    ("prefix_alpha", "prefix_beta"),
    // Pure insertion and pure deletion — no counterpart token at all.
    ("let x = 1;", "let x = 1; // note"),
    ("let x = 1; // note", "let x = 1;"),
    // Whitespace is its own token, so re-indentation is visible rather than
    // absorbed into the token beside it.
    ("    indented", "        indented"),
    // Saturation: nothing in common, so the word tint is dropped and the line
    // tint carries the signal alone.
    ("completely different", "nothing alike here"),
    // Just under saturation, so the highlight survives — the boundary the
    // threshold actually decides.
    (
        "keep this part the same but change the tail",
        "keep this part the same but change the end",
    ),
    // Multi-run: two separate changes, which must NOT be refined at character
    // granularity (that is what produces a speckled highlight).
    ("a 1 b 2 c", "a 9 b 8 c"),
    // Empty sides.
    ("", "added"),
    ("removed", ""),
    // Non-ASCII: `is_alphanumeric` is Unicode-aware, and every length here is
    // counted in code points. A port using UTF-16 units saturates differently.
    ("café ünïcode", "café ünicode"),
    // Astral plane — one emoji is one char to Rust and two code units to
    // JavaScript, so this row is the witness for the port counting correctly.
    ("status: ✅ done", "status: ❌ done"),
    ("a 🎉 b", "a 🎈 b"),
    // Realistic JSON and Rust, the two shapes this actually renders.
    (
        r#"{"model": "claude-opus-5", "effort": "high"}"#,
        r#"{"model": "claude-opus-5", "effort": "medium"}"#,
    ),
    (
        "pub fn resolve(&self, id: &str) -> Option<Pin> {",
        "pub fn resolve(&self, id: &TaskId) -> Option<Pin> {",
    ),
];

/// Render the whole matrix as the text both languages must agree on.
///
/// Deliberately a flat, greppable line per case rather than JSON: the file is
/// read by a human deciding whether a re-bless is right, and `|plain|` against
/// `[changed]` says what moved at a glance. Both delimiters are escaped out of
/// the span text so a line containing one cannot forge a boundary.
fn render_matrix() -> String {
    let mut out = String::new();
    out.push_str(
        "# stella_transcript::word::highlight over a fixed matrix.\n\
         # Generated — re-bless with:\n\
         #   BLESS=1 cargo test -p stella-transcript --test word_highlight_matrix\n\
         # The TypeScript port must reproduce this byte for byte:\n\
         #   node arenabench/ui/scripts/check-word-diff-parity.mjs\n\
         # Spans: |unchanged| and [changed]. `\\n` `\\|` `\\[` `\\]` are escaped.\n",
    );
    for (index, &(old, new)) in MATRIX.iter().enumerate() {
        let (old_spans, new_spans) = stella_transcript::word::highlight(old, new);
        out.push_str(&format!(
            "case={index} old={} new={}\n",
            render_spans(&old_spans),
            render_spans(&new_spans),
        ));
    }
    out
}

fn render_spans(spans: &[stella_transcript::word::Span]) -> String {
    spans
        .iter()
        .map(|span| {
            let text = escape(&span.text);
            if span.changed {
                format!("[{text}]")
            } else {
                format!("|{text}|")
            }
        })
        .collect()
}

fn escape(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '\\' => "\\\\".to_string(),
            '\n' => "\\n".to_string(),
            '|' => "\\|".to_string(),
            '[' => "\\[".to_string(),
            ']' => "\\]".to_string(),
            _ => ch.to_string(),
        })
        .collect()
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/word-highlight-matrix.txt")
}

#[test]
fn word_highlighting_still_matches_its_committed_matrix() {
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
    // Name the first differing line rather than dumping two blobs: the answer
    // to "what did I change about the policy" is one row.
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
         and then port it to arenabench/ui/lib/word-diff.ts, which is checked\n\
         against this same file by\n  \
         node arenabench/ui/scripts/check-word-diff-parity.mjs",
        path.display()
    );
}
