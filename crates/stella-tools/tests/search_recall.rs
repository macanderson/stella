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

use std::path::PathBuf;

use stella_tools::registry::Tool;

/// One labelled query: what to ask, and what a correct answer contains.
struct Probe {
    /// The query, exactly as a model would phrase it.
    query: &'static str,
    /// A workspace-relative path that MUST appear in the ranked answer.
    expect: &'static str,
    /// How near the top it must appear. **Presence is not the bar** — #3089
    /// asks for the top 3, and an answer buried at rank 36 of 139 is one the
    /// model will never read. Asserting only presence is how a harness
    /// reports success over a result nobody could use.
    within: usize,
    /// Why this probe exists — printed with the result, so a failure reads as
    /// a finding rather than as a red line.
    why: &'static str,
}

/// The three #3089 names, which test three different things.
const PROBES: &[Probe] = &[
    Probe {
        query: "where is the prompt cache engaged on outbound requests",
        expect: "crates/stella-model/src/anthropic.rs",
        within: 3,
        why: "the measured regression: file-level vectors put the answer \
              nowhere in the top 10 and spread only 0.05 across all of them",
    },
    Probe {
        query: "the function that decides whether a ranked list stops being relevant",
        expect: "crates/stella-embed/src/rank.rs",
        within: 5,
        why: "an answer that is exactly one function — does chunk granularity \
              resolve to a symbol rather than to its file?",
    },
    Probe {
        query: "why must a new AgentEvent variant declare what consumes it",
        expect: "AGENTS.md",
        within: 5,
        why: "an answer that is PROSE, not code — unanswerable from the code \
              index at all, so a pass cannot be an accident of it",
    },
    Probe {
        query: "SearchConfig",
        expect: "crates/stella-tools/src/search.rs",
        within: 1,
        why: "#3125: a bare symbol name is a certainty, not a ranking \
              problem — the defining file must be rank 1 even when the \
              semantic rung returns most of the corpus",
    },
];

fn workspace() -> PathBuf {
    std::env::var("STELLA_SEARCH_WORKSPACE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[tokio::test]
async fn search_returns_the_file_that_answers_the_query() {
    if std::env::var("STELLA_SEARCH_RECALL").as_deref() != Ok("1") {
        println!("SKIPPED: set STELLA_SEARCH_RECALL=1 to run (needs a live embedder)");
        return;
    }
    let root = workspace();
    println!("workspace: {}", root.display());

    let mut failures = Vec::new();
    for probe in PROBES {
        // Through the `Tool` trait, which is the path the agent itself takes
        // — so a schema or dispatch defect shows up here rather than being
        // stepped around by calling the inner function.
        let tool = stella_tools::search::Search::from_env();
        let output = tool
            .execute(&serde_json::json!({ "query": probe.query }), &root)
            .await;
        let content = match output {
            stella_protocol::tool::ToolOutput::Ok { content } => content,
            stella_protocol::tool::ToolOutput::Error { message } => {
                failures.push(format!("`{}` errored: {message}", probe.query));
                continue;
            }
        };

        println!("\n=== query: {}\n=== why:   {}", probe.query, probe.why);
        println!("{content}");

        // Rank, not presence. The hit lines are the ones that start at
        // column 0 and look like a path; the renderer indents everything else.
        let ranked: Vec<&str> = content
            .lines()
            .filter(|line| {
                !line.starts_with(char::is_whitespace)
                    && (line.contains(".rs") || line.contains(".md") || line.contains(".py"))
                    && !line.starts_with("search `")
            })
            .collect();
        match ranked.iter().position(|line| line.trim() == probe.expect) {
            Some(index) if index < probe.within => {
                println!("PASS: `{}` at rank {}", probe.expect, index + 1);
            }
            Some(index) => failures.push(format!(
                "`{}` returned `{}` at rank {} of {} — the bar is top {}\n  ({})",
                probe.query,
                probe.expect,
                index + 1,
                ranked.len(),
                probe.within,
                probe.why
            )),
            None => failures.push(format!(
                "`{}` did not return `{}` at all (in {} results)\n  ({})",
                probe.query,
                probe.expect,
                ranked.len(),
                probe.why
            )),
        }
    }

    assert!(
        failures.is_empty(),
        "search failed to answer {} of {} probes:\n{}",
        failures.len(),
        PROBES.len(),
        failures.join("\n")
    );
}
