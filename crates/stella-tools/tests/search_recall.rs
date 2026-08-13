// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Does `search` actually find the answer? (#3097)
//!
//! Every other test of this tool uses a hashing embedder, which declares
//! [`SimilarityPosture::Surface`] — its scores order candidates and certify
//! nothing. That is precisely the property under test here, so those tests
//! cannot answer the only question that matters: **given a real embedding
//! backend and a real workspace, does the tool return the file that answers
//! the query?**
//!
//! This harness runs against a live backend and a real index, so it is gated
//! on `STELLA_SEARCH_RECALL=1` **and** a configured embedder. Without both it
//! prints why it did not run and passes — a skip that silently reads as a pass
//! is the failure shape #3011 already paid for, so the reason is always
//! printed.
//!
//! ```sh
//! set -a; . ~/.env.global.local; set +a
//! STELLA_SEARCH_RECALL=1 STELLA_SEARCH_WORKSPACE=/path/to/stella \
//!   cargo test -p stella-tools --test search_recall -- --nocapture
//! ```
//!
//! # Why the corpus is a parameter
//!
//! The three built-in probes below are about *this* workspace, and this
//! workspace is 29,334 chunks — far more than one run can embed, so they are
//! necessarily graded against a **partial** index. That confounds the two
//! questions a recall harness has to keep apart: *did ranking put the answer
//! on top*, and *was the answer even in the index yet*. A miss on a partial
//! index is inconclusive by construction (`coverage_note` says exactly this in
//! the tool's own output), so a harness that can only run here can never
//! isolate ranking quality.
//!
//! Two knobs fix that without changing what the built-ins assert:
//!
//! - `STELLA_SEARCH_PROBES=<file.json>` grades a **different corpus** — one
//!   small enough to index completely — against probes written for it.
//! - `STELLA_SEARCH_WARM=<n>` runs up to `n` warm-up searches first. Embedding
//!   fills in path order a batch per call, so a small workspace reaches full
//!   coverage in a few passes and the tool stops emitting `PARTIAL INDEX`.
//!   Default `0`: the built-in run costs exactly what it always did.
//!
//! Whether the index was complete **at grading time** is printed either way,
//! because a recall number over a partial index is not the same measurement
//! and must never be reported as if it were.
//!
//! ```sh
//! STELLA_SEARCH_RECALL=1 \
//!   STELLA_SEARCH_WORKSPACE=/path/to/small-repo \
//!   STELLA_SEARCH_PROBES=crates/stella-tools/tests/fixtures/probes-cgp.json \
//!   STELLA_SEARCH_WARM=40 \
//!   cargo test -p stella-tools --test search_recall -- --nocapture
//! ```

use std::path::PathBuf;

use stella_tools::registry::Tool;

/// One labelled query: what to ask, and what a correct answer contains.
struct Probe {
    /// The query, exactly as a model would phrase it.
    query: String,
    /// Workspace-relative paths, **any one of which** is a correct answer.
    ///
    /// A set rather than a single path because in a real repository a concept
    /// legitimately has more than one home — a type and the ADR that decided
    /// it are both right, and insisting on one of them would grade the
    /// harness's taste rather than the tool's recall. Every member is
    /// preregistered before the run for the same reason.
    any_of: Vec<String>,
    /// How near the top it must appear. **Presence is not the bar** — #3089
    /// asks for the top 3, and an answer buried at rank 36 of 139 is one the
    /// model will never read. Asserting only presence is how a harness
    /// reports success over a result nobody could use.
    within: usize,
    /// Why this probe exists — printed with the result, so a failure reads as
    /// a finding rather than as a red line.
    why: String,
}

/// The three #3089 names, which test three different things.
fn builtin_probes() -> Vec<Probe> {
    vec![
        Probe {
            query: "where is the prompt cache engaged on outbound requests".into(),
            any_of: vec!["crates/stella-model/src/anthropic.rs".into()],
            within: 3,
            why: "the measured regression: file-level vectors put the answer nowhere in the top \
                  10 and spread only 0.05 across all of them"
                .into(),
        },
        Probe {
            query: "the function that decides whether a ranked list stops being relevant".into(),
            any_of: vec!["crates/stella-embed/src/rank.rs".into()],
            within: 5,
            why: "an answer that is exactly one function — does chunk granularity resolve to a \
                  symbol rather than to its file?"
                .into(),
        },
        Probe {
            query: "why must a new AgentEvent variant declare what consumes it".into(),
            any_of: vec!["AGENTS.md".into()],
            within: 5,
            why: "an answer that is PROSE, not code — unanswerable from the code index at all, \
                  so a pass cannot be an accident of it"
                .into(),
        },
        Probe {
            query: "SearchConfig".into(),
            any_of: vec!["crates/stella-tools/src/search.rs".into()],
            within: 1,
            why: "#3125: a bare symbol name is a certainty, not a ranking problem — the defining \
                  file must be rank 1 even when the semantic rung returns most of the corpus"
                .into(),
        },
    ]
}

/// The probe set for this run: the built-ins, or a corpus-specific file.
///
/// Parsed through [`serde_json::Value`] rather than a derived type because the
/// file is a test fixture read in one place, and a dependency added to read
/// four fields is a dependency the whole workspace then carries.
fn probes() -> Result<Vec<Probe>, String> {
    let Ok(path) = std::env::var("STELLA_SEARCH_PROBES") else {
        return Ok(builtin_probes());
    };
    let raw = std::fs::read_to_string(&path)
        .map_err(|error| format!("STELLA_SEARCH_PROBES=`{path}` could not be read: {error}"))?;
    let parsed: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| format!("STELLA_SEARCH_PROBES=`{path}` is not valid JSON: {error}"))?;
    let entries = parsed
        .as_array()
        .ok_or_else(|| format!("STELLA_SEARCH_PROBES=`{path}` must hold a JSON array"))?;

    let mut probes = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let text = |key: &str| entry.get(key).and_then(serde_json::Value::as_str);
        let query = text("query")
            .ok_or_else(|| format!("probe {index} in `{path}` has no string `query`"))?;
        let any_of: Vec<String> = entry
            .get("any_of")
            .and_then(serde_json::Value::as_array)
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if any_of.is_empty() {
            return Err(format!(
                "probe {index} in `{path}` has no `any_of` paths — a probe that accepts nothing \
                 cannot pass or fail, it can only look like it ran"
            ));
        }
        probes.push(Probe {
            query: query.to_string(),
            any_of,
            within: entry
                .get("within")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(3) as usize,
            why: text("why").unwrap_or("(no rationale recorded)").to_string(),
        });
    }
    Ok(probes)
}

fn workspace() -> PathBuf {
    std::env::var("STELLA_SEARCH_WORKSPACE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// The tool's own words for an index it has not finished filling.
const PARTIAL: &str = "PARTIAL INDEX";

/// The ranked paths, in order, from one rendered answer.
///
/// A hit block is a path at column 0 followed by its indented `why:` line
/// (`enrich::render_hit`), which is what distinguishes it from the header, the
/// coverage note, and every facet line. Keying on that shape rather than on a
/// list of file extensions is what lets the same harness grade a corpus of
/// `.ts`, `.py` and `.ndjson` files.
fn ranked_paths(content: &str) -> Vec<&str> {
    let lines: Vec<&str> = content.lines().collect();
    lines
        .windows(2)
        .filter_map(|pair| {
            let (head, next) = (pair[0], pair[1]);
            let is_head = !head.is_empty() && !head.starts_with(char::is_whitespace);
            let is_why =
                next.starts_with(char::is_whitespace) && next.trim_start().starts_with("why:");
            (is_head && is_why).then_some(head)
        })
        .collect()
}

/// Run `search` once and hand back what it rendered.
async fn ask(root: &std::path::Path, query: &str) -> Result<String, String> {
    let tool = stella_tools::search::Search::from_env();
    match tool
        .execute(&serde_json::json!({ "query": query }), root)
        .await
    {
        stella_protocol::tool::ToolOutput::Ok { content } => Ok(content),
        stella_protocol::tool::ToolOutput::Error { message } => Err(message),
    }
}

/// Embed until the tool stops calling the index partial, until warming stops
/// making progress, or until `budget` passes are spent.
///
/// Returns whether the index was complete when warming stopped. It is a
/// return value rather than an assertion because the built-in corpus can never
/// reach complete and must still be gradeable — the caller decides what an
/// incomplete index means for the run it is doing.
///
/// **A repeated note ends the loop.** `coverage_note` compares a chunk-vector
/// count against a symbol count, and those are not the same population: chunk
/// rows are keyed `(file_id, chunk_sha256, fingerprint)`, so two byte-identical
/// bodies in one file — two trait accessors that both read `&self.id` — store
/// one row for two symbols. The comparison is then unsatisfiable and the loop
/// would spend its whole budget re-embedding nothing (#3124). Stopping at the
/// fixed point reports the real ceiling instead of timing out at it.
async fn warm_index(root: &std::path::Path, budget: usize) -> bool {
    let mut previous: Option<String> = None;
    for pass in 1..=budget {
        match ask(root, "the main entry point of this project").await {
            Ok(content) if !content.contains(PARTIAL) => {
                println!("index complete after {pass} warm-up pass(es)");
                return true;
            }
            Ok(content) => {
                let Some(note) = content
                    .lines()
                    .find(|line| line.contains(PARTIAL))
                    .map(str::trim)
                    .map(str::to_string)
                else {
                    continue;
                };
                println!("warm-up pass {pass}: {note}");
                if previous.as_deref() == Some(note.as_str()) {
                    println!(
                        "warm-up stopped at pass {pass}: coverage stopped changing, so this is \
                         the index's real ceiling and further passes embed nothing"
                    );
                    return false;
                }
                previous = Some(note);
            }
            Err(message) => {
                println!("warm-up pass {pass} errored: {message}");
                return false;
            }
        }
    }
    false
}

#[tokio::test]
async fn search_returns_the_file_that_answers_the_query() {
    if std::env::var("STELLA_SEARCH_RECALL").as_deref() != Ok("1") {
        println!("SKIPPED: set STELLA_SEARCH_RECALL=1 to run (needs a live embedder)");
        return;
    }
    let root = workspace();
    println!("workspace: {}", root.display());

    let probes = match probes() {
        Ok(probes) => probes,
        Err(reason) => panic!("{reason}"),
    };
    println!("probes: {}", probes.len());

    let warm_budget = std::env::var("STELLA_SEARCH_WARM")
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let complete = if warm_budget > 0 {
        warm_index(&root, warm_budget).await
    } else {
        println!("no warm-up requested (STELLA_SEARCH_WARM=0)");
        false
    };

    let mut failures = Vec::new();
    let mut graded_partial = 0usize;
    for probe in &probes {
        // Through the `Tool` trait, which is the path the agent itself takes
        // — so a schema or dispatch defect shows up here rather than being
        // stepped around by calling the inner function.
        let content = match ask(&root, &probe.query).await {
            Ok(content) => content,
            Err(message) => {
                failures.push(format!("`{}` errored: {message}", probe.query));
                continue;
            }
        };
        if content.contains(PARTIAL) {
            graded_partial += 1;
        }

        println!("\n=== query: {}\n=== why:   {}", probe.query, probe.why);
        println!("{content}");

        // Rank, not presence.
        let ranked = ranked_paths(&content);
        println!(
            "--- top 5: {}",
            ranked
                .iter()
                .take(5)
                .enumerate()
                .map(|(index, path)| format!("{}. {path}", index + 1))
                .collect::<Vec<_>>()
                .join("  ")
        );
        match ranked
            .iter()
            .position(|line| probe.any_of.iter().any(|want| line.trim() == want))
        {
            Some(index) if index < probe.within => {
                println!("PASS: `{}` at rank {}", ranked[index].trim(), index + 1);
            }
            Some(index) => failures.push(format!(
                "`{}` returned `{}` at rank {} of {} — the bar is top {}\n  ({})",
                probe.query,
                ranked[index].trim(),
                index + 1,
                ranked.len(),
                probe.within,
                probe.why
            )),
            None => failures.push(format!(
                "`{}` did not return any of {:?} at all (in {} results)\n  ({})",
                probe.query,
                probe.any_of,
                ranked.len(),
                probe.why
            )),
        }
    }

    // Always stated, pass or fail: recall over a partial index is a different
    // measurement, and the number is worthless without which one it is.
    println!(
        "\nindex at grading time: {} ({} of {} probes graded against a partial index)",
        if complete {
            "COMPLETE"
        } else {
            "not proven complete"
        },
        graded_partial,
        probes.len()
    );

    assert!(
        failures.is_empty(),
        "search failed to answer {} of {} probes:\n{}",
        failures.len(),
        probes.len(),
        failures.join("\n")
    );
}
