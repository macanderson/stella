// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The latency measurement #4385 asks for: replay the sixty-one `search`
//! calls one recorded session made, against today's code and a real backend,
//! and print the distribution.
//!
//! #4385 reported the tool at **6 935 ms median, 21 361 ms p90, 33 450 ms
//! max** over `tool_calls.duration_ms` for executions 225/242/248 of session
//! `ses-1787465453163-60967`, and closed with "p90 under a stated target,
//! measured by a bench loop over the 61 recorded queries". A number read off
//! a recording cannot be compared against a number read off different queries,
//! so the queries themselves are committed beside this file
//! (`fixtures/recorded_search_queries.txt`, extracted verbatim from that
//! session's `tool_calls.args_json`) and this harness is the loop.
//!
//! # How to run it
//!
//! ```text
//! VOYAGE_API_KEY=pa-… STELLA_SEARCH_REPLAY_ROOT=/path/to/an/indexed/checkout \
//!     cargo test -p stella-tools --release --test search_latency_replay \
//!     -- --ignored --nocapture
//! ```
//!
//! `--release` is not optional and neither is `--nocapture`: the ranking is a
//! full scan over every stored vector, so a debug build measures the compiler
//! rather than the tool, and the distribution is the output.
//!
//! `STELLA_SEARCH_REPLAY_ROOT` names a checkout whose
//! `.stella/private/codegraph.db` is already **filled with vectors under the
//! same embedder the run resolves**. Filling it is `search::backfill`'s job
//! and is off this path by design (#4043) — a replay against an empty index
//! measures the name and scan rungs, which is not what #4385 is about, so the
//! harness refuses rather than reporting a fast meaningless number.
//! `STELLA_WORKSPACE_STATE_ROOT` (`stella_home::WORKSPACE_STATE_ROOT_ENV`)
//! redirects where that index is read from, which is how a run measures
//! against a copy instead of a live session's database.
//!
//! # What it does not measure
//!
//! Not the recorded numbers. Those came off a loaded laptop — the same
//! session's `bash` calls averaged 8 s — so they are a fact about that machine
//! under that load, and this harness measures **this code path on whatever
//! machine runs it**. It prints the recorded row beside its own so the two are
//! never read as one measurement.
//!
//! # Status — attempted 2026-08-24, and what came back
//!
//! **No distribution.** The attempt is recorded here rather than omitted,
//! because "we tried and the machine could not answer" is a different state
//! from "nobody has run it", and only one of them is what #4385 is waiting on.
//!
//! The run went out over this repository's own index (1 585 file vectors,
//! 28 813 chunk vectors, `voyage-code-3@1/1024/l2`) on an M-series laptop that
//! was also hosting several agent worktrees. Its load average rose from 24 to
//! 64 during the run and the per-query wall clock rose monotonically with it,
//! from 15 422 ms on the first query to 201 500 ms on the fiftieth. A series
//! that climbs 13x while the machine's load triples underneath it measures the
//! machine. **Median and p90 over it would be arithmetic, not measurement**,
//! so none is quoted, and #4385's "p90 under a stated target" is still open.
//!
//! What that run did establish, neither part depending on load:
//!
//! - **Every query came back 200+ hits deep** (203-256 across the fifty), so
//!   the relevance boundary is running to the end of the merged ranking on
//!   effectively every query against this corpus. That is the condition
//!   `engine::ceiling_note` fires on, which is why this harness prints each
//!   answer's note: #4385's first half is about what the caller reads.
//! - **A call was paying two full passes over the database.** The ranking
//!   scan reads and decodes 118 MB of chunk vectors, which is inherent to a
//!   brute-force rank; on top of it `CodeGraph::open` ran `PRAGMA
//!   quick_check` over all 180 MB of the image, on **every** call, because
//!   `report_with` opens the graph per call. The second one is gone (#4385,
//!   `stella_graph::store::image_check`) and is the only one this repository
//!   has removed.
//!
//! The component costs behind a call, measured separately against the same
//! index while the machine was at load 24: `index_all` catch-up 78-97 ms warm
//! (413 ms with 39 files to re-parse), the query's embedding round trip
//! 111-202 ms against `api.voyageai.com`, the file rank 10 ms, the chunk rank
//! 220-296 ms, and a standalone `PRAGMA quick_check` 333 ms. Those are the
//! terms; what a quiet machine adds up to is what the next run of this
//! harness should record.

use std::path::{Path, PathBuf};
use std::time::Instant;

use stella_tools::search::engine::{SearchConfig, report_with};

/// Named in the `#[ignore]` reason and in the panic, so the missing piece is
/// stated the same way whichever path the reader arrives on.
const NO_BACKEND: &str = "no embedding backend is configured: set VOYAGE_API_KEY (a `pa-` key; an \
                          `al-` Atlas key gets HTTP 403 from api.voyageai.com), or OPENAI_API_KEY, \
                          or STELLA_EMBED_URL together with STELLA_EMBED_MODEL";

/// Which checkout to search. Its index is the corpus.
const ROOT_ENV: &str = "STELLA_SEARCH_REPLAY_ROOT";

/// How many of the recorded queries to replay, for a run that is asking what
/// a call *returns* rather than what the whole set costs. Absent, all of them.
const LIMIT_ENV: &str = "STELLA_SEARCH_REPLAY_LIMIT";

/// The queries, one per line, exactly as the recorded session sent them.
const RECORDED: &str = include_str!("fixtures/recorded_search_queries.txt");

/// What the recording said, printed beside what this run says so the two are
/// never read as one measurement.
const RECORDED_MEDIAN_MS: u64 = 6_935;
const RECORDED_P90_MS: u64 = 21_361;
const RECORDED_MAX_MS: u64 = 33_450;

/// The `numerator`/`denominator` percentile of a sorted list, nearest-rank —
/// the definition the recorded row was summarised under, so the two rows
/// compare. A fraction rather than a float because the rank is an index and
/// rounding an index through `f64` is a way to be off by one silently.
fn percentile(sorted: &[u128], numerator: usize, denominator: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (numerator * sorted.len())
        .div_ceil(denominator)
        .clamp(1, sorted.len());
    sorted[rank - 1]
}

fn replay_root() -> PathBuf {
    let root = std::env::var_os(ROOT_ENV).map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(Path::parent)
                .expect("crates/<crate> always has two ancestors")
                .to_path_buf()
        },
        PathBuf::from,
    );
    assert!(
        root.join("Cargo.toml").is_file(),
        "{} is not a checkout — set {ROOT_ENV} to one whose index is filled",
        root.display()
    );
    root.canonicalize().expect("canonicalize the replay root")
}

#[tokio::test]
#[ignore = "needs a real embedding backend and a checkout whose code-graph index is already \
            filled; set VOYAGE_API_KEY (or STELLA_EMBED_URL + STELLA_EMBED_MODEL) and \
            STELLA_SEARCH_REPLAY_ROOT and run with --release --ignored --nocapture"]
async fn replay_the_recorded_queries_and_print_the_latency_distribution() {
    // Asked for explicitly and unable to run: a failure, never a pass (#3011).
    let resolution = stella_embed::from_env();
    match &resolution {
        stella_embed::Resolution::Configured(_) => {}
        stella_embed::Resolution::Unconfigured => panic!("{NO_BACKEND}"),
        stella_embed::Resolution::Incomplete(reason) => {
            panic!("the embedding backend is half-configured: {reason}\n{NO_BACKEND}")
        }
    }

    let root = replay_root();
    let limit = std::env::var(LIMIT_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    let queries: Vec<&str> = RECORDED
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(limit)
        .collect();
    assert!(
        !queries.is_empty(),
        "the recorded query fixture is empty, so there is nothing to replay"
    );

    // The session-lifetime cache the `search` TOOL keeps, not the one-shot
    // door's fresh one: replaying with a fresh cache per query would measure a
    // surface no agent uses.
    let cache = std::sync::Mutex::new(stella_tools::search::cache::GatherCache::default());
    let config = SearchConfig::default();

    println!("\n=== search latency replay =========================================");
    println!("root      {}", root.display());
    println!("queries   {}", queries.len());

    let mut millis: Vec<u128> = Vec::with_capacity(queries.len());
    let mut empty = 0usize;
    for (index, query) in queries.iter().enumerate() {
        let started = Instant::now();
        let report = report_with(
            &root,
            query,
            config,
            // Resolved per call, exactly as `report_cached` does: the
            // resolution is part of what a call pays for.
            stella_embed::from_env(),
            &cache,
        )
        .await;
        let elapsed = started.elapsed();
        millis.push(elapsed.as_millis());
        if report.hits.is_empty() {
            empty += 1;
        }
        println!(
            "  {:>3}. {:>6} ms  {:>3} hit(s)  {query}",
            index + 1,
            elapsed.as_millis(),
            report.hits.len(),
        );
        // #4385's first half is about what the caller *reads*, not what the
        // call costs: the banner that fired on 60 of its 61 recorded calls
        // and taught one model to use `rg` instead. Printed per query so a
        // run answers that question without a stopwatch.
        if let Some(note) = &report.note {
            println!("       note: {note}");
        }
    }

    // A run that ranked nothing measured the name and scan rungs, which is a
    // different tool. Reporting its distribution as the semantic path's would
    // be the surface signal CLAUDE.md forbids.
    assert!(
        empty * 2 < queries.len(),
        "{empty} of {} queries returned no hit — this index is not filled under the resolved \
         embedder, so the distribution measures the lexical rungs rather than the ranking",
        queries.len()
    );

    // #3198 asks for graph reads per call before anything is built on top of
    // the gather cache. This is that number: how many file neighborhoods the
    // whole replay took from the graph rather than from the cache.
    let gathered = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .gathered;

    millis.sort_unstable();
    println!("\n=== gather cache ==================================================");
    println!(
        "neighborhoods gathered from the graph across {} queries: {gathered}",
        queries.len()
    );
    println!("\n=== distribution ==================================================");
    println!(
        "this run        median {:>6} ms   p90 {:>6} ms   max {:>6} ms",
        percentile(&millis, 1, 2),
        percentile(&millis, 9, 10),
        millis.last().copied().unwrap_or(0),
    );
    println!(
        "recorded #4385  median {RECORDED_MEDIAN_MS:>6} ms   p90 {RECORDED_P90_MS:>6} ms   max \
         {RECORDED_MAX_MS:>6} ms"
    );
    println!(
        "\nThe two rows are different machines under different load and are not one \
         measurement. Record this run's row in this file's module doc, with the machine and \
         the index's size beside it."
    );
}
