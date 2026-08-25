// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The measurement the three search-relevance constants were never given
//! (#3096): what a real backend actually scores relevant and irrelevant
//! chunks of this repository at.
//!
//! `DEFAULT_ADMISSION_FLOOR` (`stella-embed/src/http.rs`),
//! `DEFAULT_RELEVANCE_GAP_RATIO` and `DEFAULT_MIN_BOUNDARY_GAP`
//! (`stella-embed/src/rank.rs`) each say "provisional" in their own doc
//! comment, and the floor is worse than provisional — `voyage-code-3` scored
//! *unrelated* files at 0.604 in the ranking recorded on #3089, so a floor of
//! 0.25 admits everything and drops nothing. Setting them needs a
//! distribution, and this file is the reproducible way to get one.
//!
//! # How to run it
//!
//! ```text
//! VOYAGE_API_KEY=pa-… STELLA_CALIBRATION_INDEX=/path/to/codegraph.db \
//!     cargo test -p stella-tools \
//!     --test relevance_calibration -- --ignored --nocapture
//! ```
//!
//! `--nocapture` is not optional: the distribution is the output, and libtest
//! swallows `println!` without it. Any OpenAI-shaped backend works —
//! `STELLA_EMBED_URL` + `STELLA_EMBED_MODEL` points it at a local server —
//! and resolution is `stella_embed::from_env`'s, so this test invents no
//! configuration of its own. Run it once per backend: the floor is a
//! per-`HttpEmbedder` field, so `voyage-code-3` and a local model may
//! legitimately differ.
//!
//! ## `STELLA_CALIBRATION_INDEX` — what made this affordable to run
//!
//! Without it the harness builds an index into a temporary directory and
//! embeds this whole repository first, which is the ~11M paid tokens the
//! session that wrote this file declined to spend. With it, the harness ranks
//! against an index that is **already filled under the same fingerprint** —
//! the one a working checkout's `search::backfill` pass has been filling all
//! along — and the run's entire cost is one query embedding per labelled
//! query. Four, at the time of writing.
//!
//! It changes nothing about what is measured. The distribution is a property
//! of the embedder and the corpus, and the corpus is the same rows either way;
//! what the flag removes is the re-embedding of rows that already exist. The
//! harness prints the chunk count it ranked over, so a thin index cannot be
//! mistaken for a full one, and it still refuses to report a distribution over
//! zero rows. Point it at a **copy** of a live session's database rather than
//! the live file: it opens the graph for writing (`CodeGraph::open`), and a
//! session's own indexer is already writing to that one.
//!
//! # What it prints, and what to do with it
//!
//! Per query, every candidate above [`DEPTH`] ranks deep, labelled relevant or
//! irrelevant against the answers below, with its cosine. Then, per query and
//! over the whole set:
//!
//! - **the separation** — the lowest relevant score minus the highest
//!   irrelevant one. Positive, and a floor between them drops the tail without
//!   dropping an answer. Negative, and **no floor separates this corpus**: the
//!   honest conclusion is then that the floor cannot do the job the
//!   `SimilarityPosture` contract assigns it for this backend, which is a
//!   result to write down rather than a number to tune around.
//! - **the boundary gap** — the drop across the true relevant/irrelevant
//!   frontier, as a multiple of the ranking's mean gap and in absolute
//!   cosine. Those are exactly the two tests `relevant_prefix` applies, so the
//!   two `rank.rs` constants are read straight off this column: the ratio must
//!   sit below the observed multiples and `DEFAULT_MIN_BOUNDARY_GAP` below the
//!   observed absolute drops, or the cut fires in the wrong place.
//! - **what the shipped constants would have done** — where
//!   `relevant_prefix` puts the cut today against where the labels say it
//!   belongs. A row where those agree is a constant *confirmed by
//!   measurement*, which is a result, not a non-result.
//!
//! Then edit the doc comments: replace each "provisional" paragraph with the
//! measurement and its date. **Do not delete a paragraph without measuring**
//! — that converts an honest disclosure into a silent assumption.
//!
//! # Why this asserts almost nothing
//!
//! It is a measurement, not a witness. Asserting a threshold here would be
//! tuning a constant until a favourite query looks good, which #3096's
//! constraints name as worse than the provisional value it replaced. The two
//! things it does fail on are conditions under which the *measurement itself*
//! is void: no backend when one was asked for, and a corpus that embedded
//! nothing. A missing relevant answer is data, printed, not an assertion.
//!
//! # Why `#[ignore]` and not an environment check that returns early
//!
//! Because a skip must never read as a pass (#3011). A test that inspects the
//! environment, prints a notice and returns is reported by libtest as `ok`.
//! `#[ignore = "…"]` is reported as `ignored, <reason>` — and when the test is
//! asked for explicitly with `--ignored` and no backend is configured it
//! **panics**, because at that point the absence of a key is a failure to run
//! what was asked for. The sibling harness
//! (`tests/chunk_retrieval_witnesses.rs`) carries the same two halves and its
//! module doc argues them at length.
//!
//! # Status — run 2026-08-24 against `voyage-code-3`
//!
//! What made it affordable was `STELLA_CALIBRATION_INDEX` above: the ~11M
//! paid tokens the previous session declined to spend were the *re-embedding*,
//! and an index this workspace had already filled made the run cost four query
//! embeddings.
//!
//! ```text
//! embedder    voyage-code-3@1/1024/l2      chunks 28744      depth 40
//! separation  n/a      | +0.0140 | -0.0439 | n/a
//! boundary    n/a      | +0.0140 | +0.0001 | n/a       (5.36x and 0.11x mean)
//! cut         40 vs 0  | 40 vs 1 | 40 vs 39| 40 vs 0
//! ```
//!
//! **Tightest separation -0.0439: the two distributions overlap, and no
//! admission floor separates them.** On "why is the Rust toolchain pinned to
//! an exact version" the labelled answer ranked 39th at 0.5978, under 38
//! irrelevant chunks scoring 0.6006-0.6436. Two of the four queries returned
//! no labelled answer in 40 candidates at all, which is a recall result and
//! makes their separation unmeasurable rather than good.
//!
//! What was rewritten from it, and what deliberately was not:
//!
//! - `stella_embed`'s `MEASURED_FLOORS` now records the row, and
//!   `voyage-code-3` declares `SimilarityPosture::Surface` (#2993).
//! - `DEFAULT_MIN_BOUNDARY_GAP`'s doc carries the observed drops (0.0140 and
//!   0.0001, both under its 0.05), which is why `relevant_prefix` never cuts
//!   on this corpus and why `search` still prints its RANK CEILING note
//!   (#4385).
//! - **No constant's value moved.** Two usable frontiers, pointing opposite
//!   ways, is a sample to report and not one to tune a threshold against —
//!   which is what the section above says this harness exists to avoid.
//!
//! What #3096 still wants is the other three backends
//! (`text-embedding-3-small`, `text-embedding-3-large`, a local
//! `nomic-embed-text`) and a `QUERIES` table long enough that two of its rows
//! being recall misses does not halve the sample.

use std::path::{Path, PathBuf};

use stella_embed::rank::{
    DEFAULT_MIN_BOUNDARY_GAP, DEFAULT_RELEVANCE_GAP_RATIO, Scored, relevant_prefix,
};
use stella_embed::{Embedder, Resolution, SimilarityPosture};
use stella_graph::CodeGraph;
use stella_tools::search::backfill::backfill_opened;

/// Named in the `#[ignore]` reason and in the panic, so the missing piece is
/// stated the same way whichever path the reader arrives on.
const NO_BACKEND: &str = "no embedding backend is configured: set VOYAGE_API_KEY (a `pa-` key; an \
                          `al-` Atlas key gets HTTP 403 from api.voyageai.com), or OPENAI_API_KEY, \
                          or STELLA_EMBED_URL together with STELLA_EMBED_MODEL";

/// An already-filled `codegraph.db` to rank against, instead of building and
/// embedding one. See this file's module doc for why that is a measurement
/// shortcut rather than a measurement change.
const INDEX_ENV: &str = "STELLA_CALIBRATION_INDEX";

/// How deep each ranking is measured. Deep enough that the irrelevant tail is
/// represented rather than clipped at the point the answers stop — a
/// distribution measured only where the answers are cannot show a separation.
const DEPTH: usize = 40;

/// A labelled query: the question, and the chunks whose retrieval is a
/// correct answer to it.
///
/// A candidate is relevant when its path matches `answers` and, where the
/// entry names a symbol, its chunk name matches too. Everything else the
/// ranking returns is irrelevant — which is the labelling this measurement
/// needs and is only sound because each query below has a *known, checked*
/// answer set in this repository, cited at the entry.
struct Labelled {
    query: &'static str,
    /// `(path, Some(symbol))` pins one chunk; `(path, None)` accepts any
    /// chunk of that file, for a query whose answer is a whole file's subject.
    answers: &'static [(&'static str, Option<&'static str>)],
    /// Why these are the answers and nothing else is — read at review time,
    /// and printed with the row so a later reader can re-check the label
    /// rather than trust it.
    basis: &'static str,
}

/// The labelled set. Three of the four come from #3089's definition of done,
/// whose answers were checked against this tree by
/// `tests/chunk_retrieval_witnesses.rs` and are cited there in full; the
/// fourth is added because a floor is a claim about the *tail*, and a query
/// with a broad answer set exercises a different part of the distribution
/// than three narrow ones.
const QUERIES: &[Labelled] = &[
    Labelled {
        query: "where is the prompt cache engaged on outbound requests",
        answers: &[("crates/stella-model/src/anthropic.rs", None)],
        basis: "21 `cache_control` sites plus the conversation-tail breakpoint helpers; the \
                other adapters carry a marker but not the engagement",
    },
    Labelled {
        query: "how a policy or approval decision from the hook bus becomes a content-free \
                record on the agent's event stream",
        answers: &[("crates/stella-core/src/bus.rs", Some("bridge_policy_plane"))],
        basis: "the one producer of `AgentEvent::PolicyDecision` in the tree; its file is 1,891 \
                lines about other things, so a whole-file hit is a different answer",
    },
    Labelled {
        query: "why is the Rust toolchain pinned to an exact version instead of tracking stable",
        answers: &[("AGENTS.md", None)],
        basis: "the rationale appears in exactly one place in the tree — AGENTS.md's `## \
                Essential commands`",
    },
    Labelled {
        query: "how does a provider declare which features it supports and how is that checked",
        answers: &[
            ("crates/stella-model/src/provider_parity.rs", None),
            ("AGENTS.md", None),
        ],
        basis: "invariant 8's matrix and the invariant that states it — a deliberately broader \
                answer set, so the tail is measured against something other than a single file",
    },
];

/// This repository's root — the corpus. Derived from the crate manifest so it
/// is right whatever the working directory is, and checked, because measuring
/// the wrong tree would produce a confident distribution of nothing.
fn workspace_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate> always has two ancestors")
        .to_path_buf();
    assert!(
        root.join("AGENTS.md").is_file() && root.join("Cargo.toml").is_file(),
        "{} is not this workspace's root — the corpus this measurement labels against is the \
         Stella tree itself",
        root.display()
    );
    root.canonicalize()
        .expect("canonicalize the workspace root")
}

/// One candidate as measured: where it ranked, what it scored, and whether
/// the labels call it an answer.
struct Candidate {
    label: String,
    score: f32,
    relevant: bool,
}

/// The numbers a constant is read off, for one query's ranking.
struct Measured {
    /// Lowest relevant score minus highest irrelevant score. Negative means
    /// no floor separates them.
    separation: Option<f32>,
    /// The drop across the last relevant candidate's boundary, as a multiple
    /// of the ranking's mean gap.
    boundary_ratio: Option<f32>,
    /// The same drop in absolute cosine.
    boundary_gap: Option<f32>,
    /// Where the labels put the cut, and where `relevant_prefix` puts it with
    /// the shipped constants.
    labelled_prefix: usize,
    shipped_prefix: usize,
}

/// Read the calibration numbers off a labelled ranking.
///
/// The boundary is measured at the *deepest relevant* candidate rather than
/// at the first irrelevant one: a ranking that interleaves them has no clean
/// frontier, and taking the deepest is the conservative reading — it is the
/// cut that would have to be made to keep every answer.
fn measure(candidates: &[Candidate]) -> Measured {
    let scored: Vec<Scored> = candidates
        .iter()
        .map(|c| Scored {
            key: c.label.clone(),
            score: c.score,
        })
        .collect();
    let shipped_prefix = relevant_prefix(
        &scored,
        DEFAULT_RELEVANCE_GAP_RATIO,
        DEFAULT_MIN_BOUNDARY_GAP,
    );

    let deepest_relevant = candidates.iter().rposition(|c| c.relevant);
    let labelled_prefix = deepest_relevant.map_or(0, |index| index + 1);

    let lowest_relevant = candidates
        .iter()
        .filter(|c| c.relevant)
        .map(|c| c.score)
        .fold(f32::INFINITY, f32::min);
    let highest_irrelevant = candidates
        .iter()
        .filter(|c| !c.relevant)
        .map(|c| c.score)
        .fold(f32::NEG_INFINITY, f32::max);
    let separation = (lowest_relevant.is_finite() && highest_irrelevant.is_finite())
        .then_some(lowest_relevant - highest_irrelevant);

    let gaps: Vec<f32> = candidates
        .windows(2)
        .map(|pair| (pair[0].score - pair[1].score).max(0.0))
        .collect();
    let mean = if gaps.is_empty() {
        0.0
    } else {
        gaps.iter().sum::<f32>() / gaps.len() as f32
    };
    let boundary_gap = deepest_relevant
        .filter(|index| *index < gaps.len())
        .map(|index| gaps[index]);
    let boundary_ratio = boundary_gap.filter(|_| mean > 0.0).map(|gap| gap / mean);

    Measured {
        separation,
        boundary_ratio,
        boundary_gap,
        labelled_prefix,
        shipped_prefix,
    }
}

fn show(value: Option<f32>) -> String {
    value.map_or_else(|| "n/a".to_string(), |v| format!("{v:+.4}"))
}

#[tokio::test]
#[ignore = "needs a real embedding backend and a full embedding pass over this repository; \
            set VOYAGE_API_KEY (or STELLA_EMBED_URL + STELLA_EMBED_MODEL) and run with \
            --ignored --nocapture"]
async fn print_the_relevant_and_irrelevant_score_distributions() {
    // Asked for explicitly and unable to run: a failure, never a pass.
    let embedder: Box<dyn Embedder> = match stella_embed::from_env() {
        Resolution::Configured(embedder) => embedder,
        Resolution::Unconfigured => panic!("{NO_BACKEND}"),
        Resolution::Incomplete(reason) => {
            panic!("the embedding backend is half-configured: {reason}\n{NO_BACKEND}")
        }
    };
    let embedder = embedder.as_ref();

    let root = workspace_root();
    let scratch = tempfile::tempdir().expect("tempdir for the index");
    let prefilled = std::env::var_os(INDEX_ENV).map(PathBuf::from);
    let db_path = prefilled
        .clone()
        .unwrap_or_else(|| scratch.path().join("codegraph.db"));
    let graph = CodeGraph::open(&root, &db_path).expect("open the index");
    graph.index_all().expect("index this workspace");

    let fingerprint = embedder.fingerprint().id();
    // A prefilled index is taken as given: re-running the backfill over it
    // would re-embed nothing (every row is already stored under this
    // fingerprint) but would still walk the whole pending scan, and the point
    // of the flag is that this run buys no embeddings it does not need.
    if prefilled.is_none() {
        backfill_opened(&graph, embedder, &mut |_| {}).await;
    }
    let chunks = graph
        .embedded_chunk_count(&fingerprint)
        .expect("chunk count");
    assert!(
        chunks > 0,
        "no chunk vectors are stored under {fingerprint}, so there is no distribution to \
         measure — a {INDEX_ENV} index filled by a different embedder is invisible to this one \
         rather than silently comparable"
    );

    let shipped_floor = match embedder.similarity_posture() {
        SimilarityPosture::Semantic { admission_floor } => admission_floor,
        SimilarityPosture::Surface => {
            panic!(
                "this backend declares `SimilarityPosture::Surface`, which certifies no \
                 threshold at all — there is no floor to calibrate"
            )
        }
    };

    println!("\n=== relevance calibration =========================================");
    println!("embedder    {fingerprint}");
    println!("chunks      {chunks}");
    println!(
        "index       {}",
        prefilled.as_ref().map_or_else(
            || "built and embedded for this run".to_string(),
            |path| format!("prefilled, {} ({INDEX_ENV})", path.display()),
        )
    );
    println!(
        "shipped     floor {shipped_floor}, gap ratio {DEFAULT_RELEVANCE_GAP_RATIO}, min gap {DEFAULT_MIN_BOUNDARY_GAP}"
    );
    println!("depth       {DEPTH} candidates per query\n");

    let mut summary: Vec<(&str, Measured)> = Vec::new();

    for labelled in QUERIES {
        let query_vector = embedder
            .embed(&[labelled.query.to_string()])
            .await
            .expect("embed the query")
            .remove(0)
            .vector;
        // No floor: the tail below the shipped one is exactly what a floor
        // has to be chosen against, so it must be in the measurement.
        let mut candidates: Vec<Candidate> = graph
            .rank_chunks_by_vector(&fingerprint, &query_vector, f32::NEG_INFINITY, DEPTH)
            .expect("rank chunks")
            .into_iter()
            .map(|chunk| Candidate {
                relevant: labelled.answers.iter().any(|(path, symbol)| {
                    chunk.path == *path && symbol.is_none_or(|symbol| chunk.name == symbol)
                }),
                label: format!("{} :: {} ({})", chunk.path, chunk.name, chunk.kind),
                score: chunk.score,
            })
            .collect();
        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        println!("--- {}", labelled.query);
        println!("    answers: {:?}", labelled.answers);
        println!("    basis:   {}", labelled.basis);
        for (index, candidate) in candidates.iter().enumerate() {
            println!(
                "    {:>3}. {:.6}  {}  {}",
                index + 1,
                candidate.score,
                if candidate.relevant {
                    "RELEVANT  "
                } else {
                    "irrelevant"
                },
                candidate.label
            );
        }

        let measured = measure(&candidates);
        println!(
            "    separation {}   boundary {} ({}x mean)   cut: labels {} / shipped {}",
            show(measured.separation),
            show(measured.boundary_gap),
            show(measured.boundary_ratio),
            measured.labelled_prefix,
            measured.shipped_prefix,
        );
        if measured.labelled_prefix == 0 {
            println!(
                "    NOTE: no labelled answer appeared in {DEPTH} candidates. That is a recall \
                 result, and it makes this query's separation unmeasurable rather than bad."
            );
        }
        println!();
        summary.push((labelled.query, measured));
    }

    // The floor is one number for the whole backend, so the binding
    // constraint is the worst query, not the average one.
    let tightest = summary
        .iter()
        .filter_map(|(_, m)| m.separation)
        .fold(f32::INFINITY, f32::min);
    println!("=== what these numbers say ========================================");
    if tightest.is_finite() {
        println!("tightest separation across the set: {tightest:+.4}");
        if tightest > 0.0 {
            println!(
                "  A floor strictly between the highest irrelevant score and the lowest \
                 relevant one drops the tail without dropping an answer. Take it from the \
                 per-query rows above — the floor is a per-`HttpEmbedder` field, so record it \
                 for {fingerprint} and not for embedders in general."
            );
        } else {
            println!(
                "  Relevant and irrelevant overlap: NO admission floor separates them on this \
                 corpus for {fingerprint}. That is the result. Write it into \
                 `DEFAULT_ADMISSION_FLOOR`'s doc comment rather than picking a number that \
                 looks decisive."
            );
        }
    } else {
        println!(
            "no query produced both a relevant and an irrelevant candidate, so no separation \
             was measurable — fix the labels or the corpus before reading anything else here"
        );
    }
    for (query, measured) in &summary {
        println!(
            "  cut {} vs labels {}  ratio {}  gap {}   {query}",
            measured.shipped_prefix,
            measured.labelled_prefix,
            show(measured.boundary_ratio),
            show(measured.boundary_gap),
        );
    }
    println!(
        "  `relevant_prefix` fires when the widest gap is both >= {DEFAULT_RELEVANCE_GAP_RATIO}x \
         the mean gap and >= {DEFAULT_MIN_BOUNDARY_GAP} absolute. Compare those two thresholds \
         against the ratio and gap columns above: where the shipped cut already matches the \
         labelled one, the constant is confirmed by measurement, which is a result worth \
         recording in its doc comment."
    );
    println!("===================================================================\n");

    graph.shutdown();
}
