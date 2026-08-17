// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Reading a unified diff back into [`Hunk`]s — the inverse of what the rest
//! of this crate does.
//!
//! Most diffs in this workspace are *computed* here and never leave structured
//! form. One does not: `AgentEvent::FileChange` carries git's own `-p` patch as
//! text, because it is measured by shelling to git at the turn boundary rather
//! than differed in-process. Every surface that renders that event therefore
//! has to read the text — and until this module existed each one read it
//! privately, with its own idea of what a header is.
//!
//! # Structural header detection
//!
//! `+++ `/`--- ` are file headers *before the first hunk of a file* and source
//! lines everywhere else. This is not pedantry: a removed line whose own text
//! begins `-- ` arrives as `--- ` once the diff prefixes it, textually
//! identical to a real header, and a parser that goes by prefix alone silently
//! drops it from the diff. Only "have we seen a `@@` yet" separates them, so
//! that is what this tracks.
//!
//! Anything unparseable degrades to fewer hunks, never to a panic and never to
//! a wrong line number: a malformed `@@` starts no hunk, so its body is
//! skipped rather than numbered against the previous hunk's coordinates.

use crate::{DiffLine, Hunk, Op};

/// Parse unified-diff text into hunks.
///
/// Body lines before the first `@@` are ignored, as is `\ No newline at end of
/// file` — it describes the file, not a line of it, and counting it would
/// shift every number after it by one.
///
/// Line semantics follow [`str::lines`], so a trailing newline adds no line.
#[must_use]
pub fn hunks(text: &str) -> Vec<Hunk> {
    let mut out: Vec<Hunk> = Vec::new();
    let mut open: Option<Hunk> = None;
    let mut seen_hunk = false;
    for line in text.lines() {
        if line.starts_with("@@") {
            if let Some(done) = open.take() {
                out.push(done);
            }
            // A malformed header opens nothing, so the body under it is
            // skipped rather than attributed to the hunk above.
            open = header(line);
            seen_hunk = true;
            continue;
        }
        if line.starts_with("diff ") {
            // A new file section: `+++`/`---` are headers again until the next
            // `@@`.
            if let Some(done) = open.take() {
                out.push(done);
            }
            seen_hunk = false;
            continue;
        }
        if line.starts_with('\\') {
            continue; // "\ No newline at end of file" — not a line of the file
        }
        if !seen_hunk {
            continue; // file metadata, or a body with no header to belong to
        }
        let Some(hunk) = open.as_mut() else {
            continue;
        };
        let (op, text) = match line.as_bytes().first() {
            Some(b'+') => (Op::Add, &line[1..]),
            Some(b'-') => (Op::Remove, &line[1..]),
            // A context line normally carries a leading space; a headerless
            // pseudo-diff's may not, so an unmarked line is context with its
            // text intact rather than a line dropped for bad punctuation.
            Some(_) => (Op::Equal, line.strip_prefix(' ').unwrap_or(line)),
            // An empty line inside a hunk is an empty context line: git emits
            // the marker, but a stripped or hand-edited diff may not.
            None => (Op::Equal, line),
        };
        hunk.lines.push(DiffLine {
            op,
            text: text.to_string(),
        });
    }
    if let Some(done) = open.take() {
        out.push(done);
    }
    // Counts are recomputed from the lines rather than trusted from the
    // header: a hand-edited or truncated diff routinely carries a header that
    // no longer describes its body, and every consumer here walks line numbers
    // from these fields.
    for hunk in &mut out {
        hunk.old_count = hunk.lines.iter().filter(|l| l.op != Op::Add).count();
        hunk.new_count = hunk.lines.iter().filter(|l| l.op != Op::Remove).count();
    }
    out.retain(|h| !h.lines.is_empty());
    out
}

/// Parse `@@ -a[,b] +c[,d] @@ …` into an empty hunk positioned at `(a, c)`.
///
/// Returns `None` when either side is missing or unparseable — the caller
/// treats that as "no hunk is open", which is what keeps a malformed header
/// from silently renumbering the lines beneath it.
fn header(line: &str) -> Option<Hunk> {
    let mut old_start = None;
    let mut new_start = None;
    for token in line.split(' ') {
        if let Some(rest) = token.strip_prefix('-') {
            old_start = rest.split(',').next().and_then(|n| n.parse().ok());
        } else if let Some(rest) = token.strip_prefix('+') {
            new_start = rest.split(',').next().and_then(|n| n.parse().ok());
        }
    }
    Some(Hunk {
        old_start: old_start?,
        old_count: 0,
        new_start: new_start?,
        new_count: 0,
        lines: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(hunk: &Hunk) -> Vec<(char, &str)> {
        hunk.lines
            .iter()
            .map(|l| (l.op.sigil(), l.text.as_str()))
            .collect()
    }

    #[test]
    fn a_git_patch_reads_back_into_positioned_hunks() {
        let text = "diff --git a/x.rs b/x.rs\n\
                    index 1234567..89abcde 100644\n\
                    --- a/x.rs\n\
                    +++ b/x.rs\n\
                    @@ -10,3 +10,3 @@ fn outer() {\n\
                    \x20keep\n\
                    -old\n\
                    +new\n";
        let hunks = hunks(text);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_start, 10);
        assert_eq!(hunks[0].new_start, 10);
        assert_eq!(
            ops(&hunks[0]),
            vec![(' ', "keep"), ('-', "old"), ('+', "new")]
        );
        assert_eq!((hunks[0].old_count, hunks[0].new_count), (2, 2));
    }

    #[test]
    fn several_hunks_and_several_files_each_keep_their_own_coordinates() {
        let text = "--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-a\n+A\n\
                    diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -90,1 +90,1 @@\n-b\n+B\n";
        let hunks = hunks(text);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].new_start, 1);
        assert_eq!(hunks[1].new_start, 90);
    }

    #[test]
    fn body_text_that_looks_like_a_file_header_is_kept_as_content() {
        // A removed line reading `-- see load.py` arrives as `--- see load.py`
        // once the diff adds its marker. A prefix-only parser drops it, and
        // the reader is shown a diff that is missing the line that mattered.
        let text = "--- a/x.sql\n+++ b/x.sql\n@@ -1,2 +1,2 @@\n--- see load.py\n+++ see load.rs\n";
        let hunks = hunks(text);
        assert_eq!(
            ops(&hunks[0]),
            vec![('-', "-- see load.py"), ('+', "++ see load.rs")],
            "both survive as body lines with their own text intact"
        );
    }

    #[test]
    fn a_headerless_pseudo_diff_parses_nothing_rather_than_guessing_positions() {
        // Bare `+`/`-` lines carry no coordinates at all. Inventing `1` would
        // put a confident, wrong line number in a gutter; the honest answer is
        // that this text has no hunks and the caller renders it as text.
        assert!(hunks("+one\n-two").is_empty());
    }

    #[test]
    fn a_malformed_header_swallows_its_body_instead_of_misnumbering_it() {
        let hunks = hunks("@@ nonsense @@\n+x\n@@ -5,1 +5,1 @@\n+y");
        assert_eq!(hunks.len(), 1, "only the well-formed hunk survives");
        assert_eq!(hunks[0].new_start, 5);
        assert_eq!(ops(&hunks[0]), vec![('+', "y")]);
    }

    #[test]
    fn the_no_newline_marker_is_not_a_line_of_the_file() {
        let hunks = hunks("@@ -1,1 +1,1 @@\n-old\n\\ No newline at end of file\n+new\n");
        assert_eq!(
            ops(&hunks[0]),
            vec![('-', "old"), ('+', "new")],
            "counting it would shift every number after it"
        );
    }

    #[test]
    fn a_header_whose_counts_disagree_with_its_body_is_corrected_not_trusted() {
        // Truncated and hand-edited diffs both produce this. Consumers walk
        // line numbers off these counts, so a stale header is a wrong gutter.
        let hunks = hunks("@@ -1,99 +1,99 @@\n ctx\n+add\n");
        assert_eq!((hunks[0].old_count, hunks[0].new_count), (1, 2));
    }

    #[test]
    fn an_empty_hunk_is_dropped_rather_than_rendered_as_a_bare_header() {
        assert!(hunks("@@ -1,0 +1,0 @@\n").is_empty());
        assert!(hunks("").is_empty());
    }

    #[test]
    fn a_parsed_diff_round_trips_through_the_differ_that_produced_it() {
        // The join that matters: what `unified_diff` renders, this reads back
        // to the same hunks. Anything less means the two halves of this crate
        // disagree about their own format.
        let old = "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\neta\ntheta";
        let new = "alpha\nBETA\ngamma\ndelta\nepsilon\nzeta\neta\nTHETA";
        let computed = crate::unified_diff(old, new, 1);
        let rendered: String = computed
            .hunks
            .iter()
            .map(|h| {
                let body: String = h
                    .lines
                    .iter()
                    .map(|l| format!("{}{}\n", l.op.sigil(), l.text))
                    .collect();
                format!("{}\n{body}", h.header())
            })
            .collect();
        assert_eq!(hunks(&rendered), computed.hunks);
    }
}
