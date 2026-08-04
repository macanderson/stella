//! The code-graph build cluster's reporting surface: what `stella init`'s
//! summary line says about the files the indexer left out.
//!
//! Split out of `agent_tests.rs` when it grew past its file-size ceiling
//! (#629) — the guard's rule is that new code goes in its own module rather
//! than onto the end of an already-oversized file, and the issue #940
//! size-limit tests below were the growth that tripped it. The whole
//! `format_graph_stats` / `index_workspace_graph_blocking` group moves
//! together: every exclusion the indexer counts is one subject, and splitting
//! the generated case from the size-limit case would leave two half-stories.
//!
//! `GraphSummary`, `format_graph_stats` and `index_workspace_graph_blocking`
//! come through the parent's `use super::*`, exactly as `engine_wiring` does.

use super::*;

/// Issue #272: `stella init`'s summary line must surface generated/minified
/// exclusion count, not let excluded files silently vanish from the totals.
/// Tested on the pure builder/output functions, never a live TTY.
#[test]
fn format_graph_stats_reports_generated_skip_count_when_nonzero() {
    let summary = GraphSummary {
        total_symbols: 12,
        total_imports: 4,
        total_files: 3,
        files_parsed: 2,
        files_unchanged: 1,
        files_skipped_generated: 5,
        files_skipped_too_large: 0,
    };
    let line = format_graph_stats(&summary);
    assert!(
        line.contains("skipped 5 generated files"),
        "line should surface the skip count: {line}"
    );
    assert!(line.contains("12 symbols"), "{line}");
}

#[test]
fn format_graph_stats_omits_the_skip_clause_when_nothing_was_skipped() {
    let summary = GraphSummary {
        total_symbols: 1,
        total_imports: 0,
        total_files: 1,
        files_parsed: 1,
        files_unchanged: 0,
        files_skipped_generated: 0,
        files_skipped_too_large: 0,
    };
    let line = format_graph_stats(&summary);
    assert!(
        !line.contains("skipped"),
        "no skip clause when nothing was excluded: {line}"
    );
}

#[test]
fn format_graph_stats_uses_singular_file_for_a_count_of_one() {
    let summary = GraphSummary {
        total_symbols: 0,
        total_imports: 0,
        total_files: 0,
        files_parsed: 0,
        files_unchanged: 0,
        files_skipped_generated: 1,
        files_skipped_too_large: 0,
    };
    let line = format_graph_stats(&summary);
    assert!(line.contains("skipped 1 generated file"), "{line}");
    assert!(
        !line.contains("generated files"),
        "singular, not plural, for a count of one: {line}"
    );
}

/// End-to-end through the real builder `stella init` calls
/// ([`index_workspace_graph_blocking`]): a `*.min.*` file sitting at the
/// workspace root (no denied directory involved) must be excluded and
/// counted, while an ordinary file alongside it indexes normally.
#[test]
fn index_workspace_graph_blocking_reports_generated_skips_end_to_end() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("app.min.js"), "function refresh(){}\n").unwrap();
    std::fs::write(ws.path().join("main.rs"), "pub fn run() {}\n").unwrap();

    let summary = index_workspace_graph_blocking(ws.path()).expect("index build succeeds");
    assert_eq!(summary.total_files, 1, "the minified file is never indexed");
    assert_eq!(summary.files_skipped_generated, 1);

    let line = format_graph_stats(&summary);
    assert!(line.contains("skipped 1 generated file"), "{line}");
}

/// Issue #940: `files_skipped_too_large` was counted by the indexer and
/// surfaced nowhere, so a user whose large file left the index got no signal
/// at all — the graph simply answered as if the file held no symbols. It rides
/// beside the generated count, and both clauses appear when both fired.
#[test]
fn format_graph_stats_reports_the_size_limit_skip_count() {
    let summary = GraphSummary {
        total_symbols: 9,
        total_imports: 2,
        total_files: 4,
        files_parsed: 4,
        files_unchanged: 0,
        files_skipped_generated: 0,
        files_skipped_too_large: 3,
    };
    let line = format_graph_stats(&summary);
    assert!(
        line.contains("skipped 3 files over the size limit"),
        "the size-cap exclusion must be visible: {line}"
    );

    let both = format_graph_stats(&GraphSummary {
        files_skipped_generated: 2,
        files_skipped_too_large: 1,
        ..summary
    });
    assert!(both.contains("skipped 2 generated files"), "{both}");
    assert!(
        both.contains("skipped 1 file over the size limit"),
        "singular, not plural, for a count of one: {both}"
    );
}

/// End-to-end through the real builder `stella init` calls: a file past the
/// indexer's size ceiling is excluded, counted, and named on the summary line,
/// while an ordinary file alongside it indexes normally.
#[test]
fn index_workspace_graph_blocking_reports_size_limit_skips_end_to_end() {
    let ws = tempfile::tempdir().unwrap();
    // Short lines of real syntax — indistinguishable from hand-written source
    // to every filter except the size cap (which stella-graph keeps private).
    let mut huge = String::new();
    let mut n = 0u32;
    while huge.len() <= 4 * 1024 * 1024 {
        huge.push_str(&format!("function generated_{n}() {{ return {n}; }}\n"));
        n += 1;
    }
    std::fs::write(ws.path().join("huge.js"), &huge).unwrap();
    std::fs::write(ws.path().join("main.rs"), "pub fn run() {}\n").unwrap();

    let summary = index_workspace_graph_blocking(ws.path()).expect("index build succeeds");
    assert_eq!(summary.files_skipped_too_large, 1);
    assert_eq!(summary.total_files, 1, "the oversize file is never indexed");

    let line = format_graph_stats(&summary);
    assert!(
        line.contains("skipped 1 file over the size limit"),
        "{line}"
    );
}
