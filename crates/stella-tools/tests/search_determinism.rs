//! The search tools must answer byte-identically to byte-identical input.
//!
//! This is not a property of `grep` alone — it is the precondition
//! `stella_core::loop_detect` runs on. That module decides whether a turn is
//! making progress by comparing tool output bytes, so a tool with any per-call
//! variation (a timestamp, a counter, an unordered walk) is invisible to loop
//! detection and can thrash until the step cap. `read_file` already needs
//! `driver::comparable_output` to strip its volatile session-tally footer;
//! this test is what keeps `grep` from ever quietly needing the same exemption.

use std::path::PathBuf;

use stella_tools::registry::Tool;

/// A small tree with several matching files, so the result spans more than one
/// path — multi-file output is where an unordered walk would show up.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for name in ["alpha.rs", "beta.rs", "gamma.rs", "delta.rs"] {
        std::fs::write(
            dir.path().join(name),
            "pub struct Deck;\nfn cache_write_tokens() -> u64 { 0 }\n// cache_write_tokens\n",
        )
        .expect("write fixture");
    }
    dir
}

#[tokio::test]
async fn repeated_identical_grep_calls_return_identical_bytes() {
    let dir = fixture();
    let root: PathBuf = dir.path().to_path_buf();
    let tool = stella_tools::grep::Grep::default();
    let input = serde_json::json!({ "pattern": "cache_write_tokens" });

    let mut outputs = Vec::new();
    for _ in 0..8 {
        outputs.push(format!("{:?}", tool.execute(&input, &root).await));
    }

    for (i, out) in outputs.iter().enumerate().skip(1) {
        assert_eq!(
            &outputs[0], out,
            "grep call {i} differed from call 0 — loop detection compares output \
             bytes, so any per-call variation blinds it to a thrashing turn"
        );
    }
    assert!(
        outputs[0].contains("alpha.rs"),
        "fixture sanity: the search must actually match: {}",
        outputs[0]
    );
}

/// `glob` shells to `fd`, which walks in parallel exactly as `rg` does — and
/// caps the walk with `--max-results`, so an unpinned walk varies the SET it
/// returns, not merely the order.
///
/// The fixture shape matters, and finding one that witnesses anything took
/// measuring: a FLAT tree of 750 files reproduces nothing, because the walk
/// drains before the threads can diverge. It takes a DEEP one, past the
/// 500-result cap, before the early stop starts landing in different places —
/// at which point it is blatant (five `fd` runs over this repo, five different
/// answers; the same tree at depth 5 below, five runs, five answers).
#[tokio::test]
async fn repeated_identical_glob_calls_return_identical_bytes() {
    /// Depth-5, fan-out-3, three files per directory: 363 directories and
    /// 1089 files, which is both well past the cap and cheap to build.
    fn build(path: &std::path::Path, depth: usize) {
        if depth == 0 {
            return;
        }
        for i in 0..3 {
            let child = path.join(format!("d{i}_{depth}"));
            std::fs::create_dir_all(&child).expect("mkdir");
            for f in 0..3 {
                std::fs::write(child.join(format!("f{f}.rs")), "fn x() {}\n").expect("write");
            }
            build(&child, depth - 1);
        }
    }
    let dir = tempfile::tempdir().expect("tempdir");
    build(dir.path(), 5);
    let root: PathBuf = dir.path().to_path_buf();
    let tool = stella_tools::glob::Glob::with_code_map();
    let input = serde_json::json!({ "pattern": "**/*.rs" });

    let first = format!("{:?}", tool.execute(&input, &root).await);
    for i in 1..8 {
        let again = format!("{:?}", tool.execute(&input, &root).await);
        assert_eq!(
            first, again,
            "glob call {i} differed — an unpinned parallel walk makes a \
             repeated search look like new information, which is exactly what \
             loop detection reads as progress"
        );
    }
    assert!(first.contains(".rs"), "fixture sanity: {first}");
}

/// The same guarantee for the modes added in #1260 — a footer, a truncation
/// note or a count that varied per call would defeat detection just as surely.
#[tokio::test]
async fn every_grep_output_mode_is_deterministic() {
    let dir = fixture();
    let root: PathBuf = dir.path().to_path_buf();
    let tool = stella_tools::grep::Grep::default();

    for mode in ["content", "files_with_matches", "count"] {
        let input = serde_json::json!({
            "pattern": "cache_write_tokens",
            "output_mode": mode,
            "context": 1,
        });
        let first = format!("{:?}", tool.execute(&input, &root).await);
        for i in 1..4 {
            let again = format!("{:?}", tool.execute(&input, &root).await);
            assert_eq!(first, again, "output_mode={mode} varied on call {i}");
        }
    }
}
