// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The three #3089 retrieval witnesses, end to end against a real embedding
//! backend (#3097).
//!
//! #3095 built the chunk index — rendering, hashing, incremental re-stamping,
//! the sweep, the relevance cut — and proved every one of those at the unit
//! level with a hashing or concept embedder. What it could not prove is the
//! thing the chunk index exists for: that a natural-language question about
//! this repository lands on the right symbol or the right paragraph. That
//! needs a real embedding backend and a populated `code_graph_chunk_vectors`,
//! and a hashing embedder cannot stand in for one —
//! `SimilarityPosture::Surface` orders candidates and certifies nothing, which
//! is exactly the property under test.
//!
//! # How to run it
//!
//! ```text
//! VOYAGE_API_KEY=pa-… cargo test -p stella-tools \
//!     --test chunk_retrieval_witnesses -- --ignored --nocapture
//! ```
//!
//! Any OpenAI-shaped backend works — `STELLA_EMBED_URL` + `STELLA_EMBED_MODEL`
//! points it at a local server. Resolution is `stella_embed::from_env`'s, so
//! this test invents no configuration of its own.
//!
//! # Why `#[ignore]` and not an environment check that returns early
//!
//! Because a skip must never read as a pass (#3011). A test that inspects the
//! environment, prints a notice and returns is reported by libtest as `ok` —
//! indistinguishable, in the one line anybody actually reads, from a run that
//! embedded eleven million tokens and asserted three witnesses. `#[ignore =
//! "…"]` is reported as `ignored, <reason>`, and the reason names the missing
//! variable. The other half of the same discipline is below: when the test is
//! asked for explicitly with `--ignored` and no backend is configured, it
//! **panics** rather than passing, because at that point the absence of a key
//! is a failure to run what was asked for, not a hermetic default.
//!
//! This is not hypothetical. `crates/stella-model/tests/live_smoke.rs` used
//! the print-and-return shape, and a default `cargo test` there reported nine
//! live-wire-shape assertions as `ok` in 0.01s having contacted nothing. It
//! now carries both halves of the discipline above (#3856).
//!
//! # The corpus is this repository, indexed into a temporary database
//!
//! `CodeGraph::open` takes the database path separately from the root, so the
//! pass reads the real tree and writes nothing into `.stella/`. #3097 sizes the
//! cold pass at ~11M tokens, under $1 at `voyage-code-3` rates — a figure taken
//! from the issue, not one this file has measured. It is paid once for all
//! three witnesses, which is why they share one test rather than being three:
//! three tests would run three cold passes in parallel against the same
//! provider. Each probe is evaluated independently and every failure is
//! reported together, so one keyed run answers all three questions — that is
//! what the threshold calibration in #3096 needs from this harness.
//!
//! # What each witness proves, and why these three
//!
//! 1. **The measured regression.** The prompt-cache question against
//!    `crates/stella-model/src/anthropic.rs`, with *both* halves #3089 names:
//!    the answer in the top 3, and a top-10 score spread over 0.05. The
//!    spread is half the defect: #3089 reports that the old file-level index
//!    put the answer nowhere in the top 10 *and* separated all ten of them by
//!    0.05 — its measurement, not this session's. A ranking that cannot
//!    distinguish its own candidates has failed even when the right file
//!    happens to sit on top of it, which is why one half does not imply the
//!    other and both are asserted.
//! 2. **Chunk granularity.** A question whose answer is exactly one function,
//!    living in a large file whose other contents are about something else.
//!    Asserting the hit's `focus` is what makes this a granularity test: a hit
//!    reached through the whole-file rung carries `focus: None`, so only a
//!    chunk hit can satisfy it. Its target was repointed once, when the file
//!    it named left the workspace — see the comment at the assertion.
//! 3. **The markdown half.** A question with no answer in the code at all.
//!
//! # Why witness 3 is the toolchain pin and not #3097's `AgentEvent` example
//!
//! #3097 offers *"why must a new `AgentEvent` variant declare what consumes
//! it"* as an example ("e.g."), on the grounds that it is unanswerable from
//! code. That is no longer quite true, and the substitute holds the property
//! more strictly rather than less:
//!
//! - `crates/stella-protocol/src/event/consumers.rs` opens with a seventy-line
//!   module doc (lines 1–73) answering that exact question in prose. It is a
//!   genuine second right answer, so pinning the markdown file above it would
//!   be asserting a ranking this test cannot justify.
//! - `AGENTS.md`'s invariants live under one 184-line heading (lines 243–426,
//!   `## Architecture: ports, not direct dependencies`), and a section is
//!   scoped to the next heading of any level. Ten invariants in one chunk is
//!   the whole-file dilution the chunk index exists to escape, reappearing at
//!   section scale.
//!
//! The toolchain-pin rationale has neither problem. `AGENTS.md` line 30 is the
//! only place in the tree that says *why* `rust-toolchain.toml` pins an exact
//! version — `grep -rn rustfmt` finds mentions elsewhere and an explanation
//! nowhere else — and it sits in a 38-line section (lines 25–62,
//! `## Essential commands`). A pass there cannot be an accident of the code
//! index, which is the property #3097 is asking for.
//!
//! # Status
//!
//! **Written, and unrun.** No embedding key was available to the session that
//! wrote this, so every threshold and every pinned target below was verified
//! by *reading* the tree (each is cited at its assertion) and none of them has
//! been observed in a ranking. #3097 is not closed by this file's existence;
//! it is closed by one green run of it with a real key.

use std::path::{Path, PathBuf};

use stella_embed::{Embedder, Resolution, SimilarityPosture};
use stella_graph::CodeGraph;
use stella_tools::search::backfill::backfill_opened;
use stella_tools::search::engine::{Answer, dispatch};

/// Named in the `#[ignore]` reason and in the panic below, so the missing
/// piece is stated the same way whichever path the reader arrives on.
const NO_BACKEND: &str = "no embedding backend is configured: set VOYAGE_API_KEY (a `pa-` key; an \
                          `al-` Atlas key gets HTTP 403 from api.voyageai.com), or OPENAI_API_KEY, \
                          or STELLA_EMBED_URL together with STELLA_EMBED_MODEL";

/// How deep the spread in witness 1 is measured. #3089 names the top ten.
const SPREAD_DEPTH: usize = 10;

/// The separator `stella_graph`'s markdown sections join their breadcrumb
/// with. Its `markdown` module is private and `BREADCRUMB_SEPARATOR` inside it
/// is `pub(crate)`, so no import can reach it and this is the one place the
/// literal is repeated; a drift shows up as a failed section assertion naming
/// the breadcrumb it actually got.
const BREADCRUMB: &str = " \u{203a} ";

/// This repository's root — the corpus. Derived from the crate manifest so it
/// is right whatever the working directory is, and checked, because indexing
/// the wrong tree would produce three confident misses with no clue why.
fn workspace_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate> always has two ancestors")
        .to_path_buf();
    assert!(
        root.join("AGENTS.md").is_file() && root.join("Cargo.toml").is_file(),
        "{} is not this workspace's root — the corpus these witnesses assert against is the \
         Stella tree itself",
        root.display()
    );
    root.canonicalize()
        .expect("canonicalize the workspace root")
}

/// The admission floor `search` itself applies, from the backend's own
/// posture — never a number this test chose.
fn floor_of(embedder: &dyn Embedder) -> f32 {
    match embedder.similarity_posture() {
        SimilarityPosture::Semantic { admission_floor } => admission_floor,
        SimilarityPosture::Surface => f32::NEG_INFINITY,
    }
}

/// One search, exactly as the `search` tool performs it.
async fn search(graph: &CodeGraph, root: &Path, embedder: &dyn Embedder, query: &str) -> Answer {
    dispatch(Some(graph), root, query, Some(embedder)).await
}

/// The scores of the best [`SPREAD_DEPTH`] candidates the two semantic rungs
/// produce, before the relevance cut.
///
/// Measured **before** the cut on purpose. The cut is a prefix of this list
/// chosen by looking for a gap in it, so it can leave fewer than ten hits —
/// and a spread computed over whatever survived would be a measurement of the
/// cut, not of the ranking. What #3089 measured, and what a flat score
/// distribution is a property of, is the ranking.
///
/// Merging the two rungs is sound here for the reason `semantic_hits` gives:
/// chunk and file vectors are written by the same embedder under the same
/// fingerprint, so they are one vector space and their cosines are directly
/// comparable.
fn top_scores(graph: &CodeGraph, fingerprint: &str, query: &[f32], floor: f32) -> Vec<f32> {
    let mut scores: Vec<f32> = graph
        .rank_chunks_by_vector(fingerprint, query, floor, SPREAD_DEPTH)
        .expect("rank chunks")
        .iter()
        .map(|chunk| chunk.score)
        .chain(
            graph
                .rank_files_by_vector(fingerprint, query, floor, SPREAD_DEPTH)
                .expect("rank files")
                .iter()
                .map(|file| file.score),
        )
        .collect();
    scores.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    scores.truncate(SPREAD_DEPTH);
    scores
}

/// An answer's ranking, one hit per line, for a failure message. A first
/// keyed run that misses should say what it found instead, in the same message.
fn render(answer: &Answer) -> String {
    if answer.hits.is_empty() {
        return "    (no hits at all)".to_string();
    }
    answer
        .hits
        .iter()
        .enumerate()
        .map(|(index, hit)| {
            format!(
                "    {:>2}. {} [{}]\n        {}",
                index + 1,
                hit.path,
                hit.focus.as_deref().unwrap_or("whole file"),
                hit.why
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
#[ignore = "needs a real embedding backend and a full embedding pass over this repository; \
            set VOYAGE_API_KEY (or STELLA_EMBED_URL + STELLA_EMBED_MODEL) and run with --ignored"]
async fn chunk_search_answers_the_three_questions_file_vectors_could_not() {
    // Asked for explicitly and unable to run: a failure, never a pass. The
    // `#[ignore]` above is the hermetic default; this is the case where
    // somebody asked for the witnesses and would otherwise be handed a green
    // tick for a run that never happened.
    let embedder: Box<dyn Embedder> = match stella_embed::from_env() {
        Resolution::Configured(embedder) => embedder,
        Resolution::Unconfigured => panic!("{NO_BACKEND}"),
        Resolution::Incomplete(reason) => {
            panic!("the embedding backend is half-configured: {reason}\n{NO_BACKEND}")
        }
    };
    let embedder = embedder.as_ref();

    let root = workspace_root();
    let db = tempfile::tempdir().expect("tempdir for the index");
    let graph = CodeGraph::open(&root, &db.path().join("codegraph.db")).expect("open the index");
    graph.index_all().expect("index this workspace");
    let indexed = graph.file_count().expect("file count");
    assert!(
        indexed > 0,
        "the workspace indexed no files at all, so nothing below measures anything"
    );

    let fingerprint = embedder.fingerprint().id();
    // The production pass, not a copy of it written for the test. Both halves
    // run to exhaustion, which is what a witness needs: a ranking over part of
    // the workspace makes its miss mean nothing.
    backfill_opened(&graph, embedder, &mut |_| {}).await;

    let chunks = graph
        .embedded_chunk_count(&fingerprint)
        .expect("chunk count");
    assert!(
        chunks > 0,
        "no chunk vectors were stored, so the three witnesses below would be measuring the \
         file-level index this issue exists to replace"
    );
    let floor = floor_of(embedder);
    let mut failures: Vec<String> = Vec::new();

    // ---- Witness 1: the measured regression -----------------------------
    //
    // `crates/stella-model/src/anthropic.rs` is where the prompt cache is
    // engaged on an outbound request: 21 `cache_control` sites, the helpers
    // that stamp the conversation-tail breakpoint, and the `anthropic-beta`
    // value that unlocks `cache_control.ttl`. It is not the *only* adapter
    // that touches a cache marker — `openai.rs`, `zai.rs` and `bedrock.rs`
    // carry one too — which is exactly why #3089 asks for the top 3 rather
    // than the top 1. Both halves of its claim are asserted; either alone
    // passes for the wrong reason.
    const CACHE_QUERY: &str = "where is the prompt cache engaged on outbound requests";
    const CACHE_ANSWER: &str = "crates/stella-model/src/anthropic.rs";
    let answer = search(&graph, &root, embedder, CACHE_QUERY).await;
    let top3: Vec<&str> = answer
        .hits
        .iter()
        .take(3)
        .map(|hit| hit.path.as_str())
        .collect();
    if !top3.contains(&CACHE_ANSWER) {
        failures.push(format!(
            "WITNESS 1 (top 3): `{CACHE_QUERY}` did not return {CACHE_ANSWER} in the top 3.\n\
             {}",
            render(&answer)
        ));
    }

    let query_vector = embedder
        .embed(&[CACHE_QUERY.to_string()])
        .await
        .expect("embed the query")
        .remove(0)
        .vector;
    let scores = top_scores(&graph, &fingerprint, &query_vector, floor);
    if scores.len() < SPREAD_DEPTH {
        failures.push(format!(
            "WITNESS 1 (spread): only {} candidate(s) cleared the admission floor {floor}, so a \
             top-{SPREAD_DEPTH} spread cannot be measured at all — scores {scores:?}",
            scores.len()
        ));
    } else {
        // The number #3089 measured: the file-level index separated all ten
        // of its candidates by 0.05, which is a ranking that cannot tell its
        // own answers apart.
        let spread = scores[0] - scores[SPREAD_DEPTH - 1];
        if spread <= 0.05 {
            failures.push(format!(
                "WITNESS 1 (spread): top-{SPREAD_DEPTH} score spread is {spread:.4}, which does \
                 not exceed 0.05 — the ranking cannot distinguish its own candidates. Scores: \
                 {scores:?}"
            ));
        }
    }

    // ---- Witness 2: the answer is exactly one function -------------------
    //
    // `bridge_policy_plane` (`crates/stella-core/src/bus.rs`) is the one
    // function in the tree that turns the hook bus's four audit event names
    // (`policy.evaluated`, `policy.blocked`, `approval.requested`,
    // `secret.detected`) into `AgentEvent::PolicyDecision` on the event
    // stream. `grep -rn PolicyDecision crates/ --include='*.rs'` finds
    // consumers everywhere — the TUI's three matchers, the CLI's
    // `diag_bridge`, `stella-tools`' `gated`/`registry` doc comments — and
    // exactly one producer, this function. Its file is 1,891 lines and on the
    // god-file list, and every other line of it is about something else:
    // subscription and unsubscription, envelope sealing, blocking-policy
    // folding, the slow-observer quarantine, bounded forwarding. So a
    // whole-file vector is diluted across all of that.
    //
    // Asserting `focus` is the granularity claim: a file-rung hit carries
    // `None` there and cannot satisfy this no matter how well the file ranks.
    //
    // This probe replaced one pinned to `ladder_decision` in
    // `crates/stella-pipeline/src/verify.rs`, which had the same shape until
    // that crate was deleted from the workspace (#3865). The corpus here is
    // the live tree, so that target became unreachable rather than merely
    // unverified — a witness that cannot pass proves nothing. Like every other
    // target in this file, the replacement was verified by reading the tree
    // (the grep above), not by observing a ranking.
    const ONE_FN_QUERY: &str = "how a policy or approval decision from the hook bus becomes a \
         content-free record on the agent's event stream";
    const ONE_FN_FILE: &str = "crates/stella-core/src/bus.rs";
    const ONE_FN_SYMBOL: &str = "bridge_policy_plane";
    let answer = search(&graph, &root, embedder, ONE_FN_QUERY).await;
    match answer.hits.first() {
        Some(hit) if hit.path == ONE_FN_FILE && hit.focus.as_deref() == Some(ONE_FN_SYMBOL) => {}
        _ => failures.push(format!(
            "WITNESS 2 (one function): `{ONE_FN_QUERY}` did not rank `{ONE_FN_SYMBOL}` in \
             {ONE_FN_FILE} first as a SYMBOL — a hit on the file with no focus is the \
             whole-file answer this witness exists to rule out.\n{}",
            render(&answer)
        )),
    }

    // ---- Witness 3: the answer is prose, not code ------------------------
    //
    // Why `rust-toolchain.toml` pins an exact version is stated in exactly one
    // place in this repository — AGENTS.md's `## Essential commands`, "each
    // new stable release ships a slightly different rustfmt". `grep -rn
    // rustfmt` over the tree finds the pin itself, comments about how rustfmt
    // lays code out, and exactly one that cites this rationale by pointing
    // back at AGENTS.md (`crates/stella-diag/tests/compile_fail.rs`) — the
    // rationale itself appears nowhere else. So a pass cannot be an accident
    // of the code index: there is no code to accidentally hit. The module doc
    // above records why this query replaced #3097's `AgentEvent` example.
    const PROSE_QUERY: &str =
        "why is the Rust toolchain pinned to an exact version instead of tracking stable";
    const PROSE_FILE: &str = "AGENTS.md";
    let prose_section = format!("AGENTS.md{BREADCRUMB}Essential commands");
    let answer = search(&graph, &root, embedder, PROSE_QUERY).await;
    match answer.hits.first() {
        Some(hit) if hit.path == PROSE_FILE && hit.focus.as_deref() == Some(&prose_section) => {}
        _ => failures.push(format!(
            "WITNESS 3 (prose): `{PROSE_QUERY}` did not rank the `{prose_section}` section of \
             {PROSE_FILE} first. A hit on {PROSE_FILE} with no focus means the markdown file \
             was reached only by its whole-file vector, which is not the markdown half this \
             witness tests.\n{}",
            render(&answer)
        )),
    }

    graph.shutdown();

    assert!(
        failures.is_empty(),
        "{} of the 3 #3089 retrieval witnesses failed against {} chunk vectors over {} files \
         (embedder {fingerprint}, admission floor {floor}):\n\n{}",
        failures.len(),
        chunks,
        indexed,
        failures.join("\n\n")
    );
}
