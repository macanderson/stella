// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `graph_query op=semantic` — ask the code graph a question in English.
//!
//! # The measurement that produced this
//!
//! On a 60-trial Terminal-Bench 2.1 head-to-head at matched model and
//! settings, Stella solved 18/20 to Claude Code's 20/20 and spent 2.1× the
//! money. The entire gap was **tool-call count** — 32.5 per trial against
//! 12.4 — and batching, round-trip clustering, cache hit rate and context
//! size were each ruled out as the cause. What the traces showed instead was
//! search by *guessing names*: on `fix-code-vulnerability`, 58 tool calls to
//! Claude Code's 7 for the same two edits, 13 of them greps and reads of a
//! single file for spelling variants of one concept
//! (`_hkey|_hval|HeaderDict|CRLF`).
//!
//! Every one of those calls is an agent describing something semantically to
//! an index that only matches substrings. This op is the missing index.
//!
//! # Where the work happens
//!
//! Deliberately split three ways, because only one third of it is allowed to
//! do I/O and only one third is allowed to make a decision:
//!
//! - `stella-graph` stores the vectors beside the nodes they describe and
//!   reports what still needs embedding. No transport, no key, no model.
//! - `stella-embed` owns the seam, the fingerprint, the HTTP backend and the
//!   pure ranker.
//! - This module is the orchestration: resolve a backend, embed what is
//!   pending, embed the query, rank, render. It is the only place the three
//!   meet, and it is where the degradation decision lives.

use std::path::Path;

use stella_embed::{Embedder, Resolution, SimilarityPosture};
use stella_graph::vectors::FileVector;
use stella_protocol::tool::ToolOutput;

use super::{OpenedGraph, open_or_build, with_index_warning};

/// Files ranked back to the model. Enough to choose from, small enough that
/// the answer is a pointer rather than a listing — the point is to replace a
/// dozen greps with one call, not to paste the repository into the prompt.
const MAX_RESULTS: usize = 10;

/// Files embedded per request to the backend. Every provider accepts a batch;
/// 32 keeps a single request well inside every documented body limit while
/// still amortising the round trip across a warm-up pass.
const BATCH: usize = 32;

/// Symbol names shown beside each hit, so the model can often skip the read
/// entirely.
const MAX_SYMBOLS_SHOWN: usize = 8;

/// Opens the answer when no semantic backend is configured and the op fell
/// back to name matching. A constant because it is the one phrase telling the
/// reader the answer is weaker than it looks, and it must not drift.
pub(crate) const LEXICAL_FALLBACK_NOTE: &str = "(no semantic embedder is configured, so this is a NAME match over paths and symbols, \
     not a meaning match — set STELLA_EMBED_URL + STELLA_EMBED_MODEL for a local model, or \
     VOYAGE_API_KEY / OPENAI_API_KEY for a hosted one)";

/// Opens the answer when a backend is configured but the request failed. The
/// answer still comes back — a broken embedding endpoint must not cost the
/// agent its turn — but it says which answer it is.
pub(crate) const DEGRADED_NOTE_PREFIX: &str =
    "(the semantic embedder failed, so this is a NAME match over paths and symbols";

/// `graph_query op=semantic` end to end.
///
/// `open_or_build` runs a full `index_all` catch-up pass, which its contract
/// says an async caller must wrap in `spawn_blocking`. The handle is then held
/// across the embedding awaits — the remaining graph calls are single SQLite
/// reads against a local file, so paying a thread hop for each would cost more
/// than it saves.
pub(crate) async fn semantic_query(root: &Path, query: &str) -> ToolOutput {
    let owned_root = root.to_path_buf();
    let opened = tokio::task::spawn_blocking(move || open_or_build(&owned_root)).await;
    let OpenedGraph {
        graph,
        index_warning,
    } = match opened {
        Ok(Ok(opened)) => opened,
        Ok(Err(message)) => return ToolOutput::Error { message },
        Err(_) => {
            return ToolOutput::Error {
                message: "the code-graph query was cancelled".into(),
            };
        }
    };

    let (embedder, configuration_note) = match stella_embed::from_env() {
        Resolution::Configured(embedder) => (Some(embedder as Box<dyn Embedder>), None),
        Resolution::Unconfigured => (None, None),
        // A half-configured backend is neither "off" nor "working". Saying so
        // is the whole reason resolution has three outcomes: a typo in a model
        // name must not read as a deliberate lexical setup.
        Resolution::Incomplete(reason) => (
            None,
            Some(format!("(semantic search is misconfigured: {reason})")),
        ),
    };

    let output = match embedder {
        Some(embedder) => semantic_query_with(&graph, embedder.as_ref(), query).await,
        None => lexical_answer(&graph, query, configuration_note.as_deref()),
    };
    graph.shutdown();
    with_index_warning(output, index_warning)
}

/// The semantic path against an explicit embedder.
///
/// The embedder is a parameter rather than something this function resolves,
/// so the wiremock test exercises the same code the tool runs — there is no
/// test-only branch anywhere below this line.
pub(crate) async fn semantic_query_with(
    graph: &stella_graph::CodeGraph,
    embedder: &dyn Embedder,
    query: &str,
) -> ToolOutput {
    let fingerprint = embedder.fingerprint().id();

    if let Err(message) = catch_up_embeddings(graph, embedder, &fingerprint).await {
        // A backend that cannot be reached is a degradation, not a turn
        // ending. Answer from names, and say which answer this is.
        return lexical_answer(
            graph,
            query,
            Some(&format!("{DEGRADED_NOTE_PREFIX}: {message})")),
        );
    }

    let query_vector = match embedder.embed(&[query.to_string()]).await {
        Ok(mut vectors) if !vectors.is_empty() => vectors.remove(0).vector,
        Ok(_) => {
            return lexical_answer(
                graph,
                query,
                Some(&format!("{DEGRADED_NOTE_PREFIX}: it returned no vector)")),
            );
        }
        Err(error) => {
            return lexical_answer(
                graph,
                query,
                Some(&format!("{DEGRADED_NOTE_PREFIX}: {error})")),
            );
        }
    };

    // Only a `Semantic` backend has earned a floor. Under `Surface` the score
    // orders and never admits, so nothing is filtered out on its say-so.
    let floor = match embedder.similarity_posture() {
        SimilarityPosture::Semantic { admission_floor } => admission_floor,
        SimilarityPosture::Surface => f32::NEG_INFINITY,
    };

    let ranked = match graph.rank_files_by_vector(&fingerprint, &query_vector, floor, MAX_RESULTS) {
        Ok(ranked) => ranked,
        Err(error) => {
            return ToolOutput::Error {
                message: format!("code-graph query failed: {error}"),
            };
        }
    };

    let embedded = graph.embedded_file_count(&fingerprint).unwrap_or(0);
    let total = graph.file_count().unwrap_or(embedded);

    if ranked.is_empty() {
        return ToolOutput::Ok {
            content: format!(
                "{} semantically relevant files for `{query}` \
                 ({embedded} of {total} files embedded by {})",
                super::UNRESOLVED_MARKER,
                embedder.fingerprint().model_id,
            ),
        };
    }

    let mut content = format!(
        "semantic matches for `{query}` — {} files ranked by {} ({embedded} of {total} embedded)",
        ranked.len(),
        embedder.fingerprint().model_id,
    );
    if embedded < total {
        content.push_str(
            "\n(embedding is incremental — files not yet embedded cannot rank; re-run to \
             continue)",
        );
    }
    for hit in &ranked {
        content.push_str(&format!("\n- {:.3}  {}", hit.score, hit.key));
        let symbols = symbol_line(graph, &hit.key);
        if !symbols.is_empty() {
            content.push_str(" — ");
            content.push_str(&symbols);
        }
    }
    ToolOutput::Ok { content }
}

/// Embed every file still pending under `fingerprint`, up to the per-pass cap.
async fn catch_up_embeddings(
    graph: &stella_graph::CodeGraph,
    embedder: &dyn Embedder,
    fingerprint: &str,
) -> Result<(), String> {
    let pending = graph
        .files_pending_embedding(fingerprint, stella_graph::MAX_FILES_PER_PASS)
        .map_err(|error| format!("cannot read the code graph: {error}"))?;
    if pending.is_empty() {
        return Ok(());
    }

    for chunk in pending.chunks(BATCH) {
        let texts: Vec<String> = chunk.iter().map(|p| p.text.clone()).collect();
        let embeddings = embedder.embed(&texts).await.map_err(|e| e.to_string())?;
        let rows: Vec<FileVector> = chunk
            .iter()
            .zip(embeddings)
            .map(|(pending, embedding)| FileVector {
                path: pending.path.clone(),
                content_sha256: pending.content_sha256.clone(),
                vector: embedding.vector,
            })
            .collect();
        graph
            .store_file_vectors(fingerprint, &rows)
            // A write failure here is not recoverable by continuing: the next
            // batch would be written into the same broken store and the pass
            // would report success having stored nothing.
            .map_err(|error| format!("cannot store file vectors: {error}"))?;
    }
    Ok(())
}

/// The honest fallback: rank indexed files by how many of the query's words
/// appear in their path or symbol names.
///
/// This is what `graph_query` could always do, said out loud. It is useful —
/// it searches symbol names, which grep over paths does not — and it is not a
/// semantic answer, which is why every caller reaching it prefixes a note.
/// Deterministic: score descending, then path ascending.
fn lexical_answer(graph: &stella_graph::CodeGraph, query: &str, note: Option<&str>) -> ToolOutput {
    let terms = terms_of(query);
    let files = match graph.all_files() {
        Ok(files) => files,
        Err(error) => {
            return ToolOutput::Error {
                message: format!("code-graph query failed: {error}"),
            };
        }
    };

    let mut scored: Vec<(usize, String, String)> = files
        .into_iter()
        .filter_map(|path| {
            let symbols = symbol_line(graph, &path);
            let haystack = format!("{path} {symbols}").to_lowercase();
            let hits = terms.iter().filter(|term| haystack.contains(*term)).count();
            (hits > 0).then_some((hits, path, symbols))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.truncate(MAX_RESULTS);

    let header = note.unwrap_or(LEXICAL_FALLBACK_NOTE);
    if scored.is_empty() {
        return ToolOutput::Ok {
            content: format!(
                "{header}\n{} files whose path or symbol names match `{query}`",
                super::UNRESOLVED_MARKER
            ),
        };
    }

    let mut content = format!("{header}\nname matches for `{query}`:");
    for (hits, path, symbols) in &scored {
        content.push_str(&format!("\n- {hits} term(s)  {path}"));
        if !symbols.is_empty() {
            content.push_str(" — ");
            content.push_str(symbols);
        }
    }
    ToolOutput::Ok { content }
}

/// English function words that carry no signal in a path or a symbol name.
///
/// A length floor alone cannot do this job: `the`, `and` and `for` are three
/// characters and pure noise, while `api`, `sql`, `url` and `log` are three
/// characters and exactly what someone is searching for. So the floor stays at
/// three and the handful of words that would otherwise match half the tree are
/// named. Deliberately short — this is a noise filter, not a linguistics
/// project, and every word added is a word a caller can no longer search for.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "was", "were", "that", "this", "with", "from", "into", "when",
    "where", "what", "which", "does", "how", "its", "not", "but", "all", "any", "can", "has",
    "have", "before", "after", "them", "then", "than",
];

/// Words worth matching on: lowercase, alphanumeric, at least three
/// characters, and not a [`STOPWORDS`] entry.
fn terms_of(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 3)
        .map(|word| word.to_lowercase())
        .filter(|word| !STOPWORDS.contains(&word.as_str()))
        .collect();
    terms.sort();
    terms.dedup();
    terms
}

/// The first few symbol names a file defines, comma-joined — often enough for
/// the model to decide whether to open the file at all.
fn symbol_line(graph: &stella_graph::CodeGraph, path: &str) -> String {
    let Ok(neighborhood) = graph.file_neighborhood(Path::new(path)) else {
        return String::new();
    };
    neighborhood
        .symbols
        .iter()
        .take(MAX_SYMBOLS_SHOWN)
        .map(|symbol| symbol.name.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests;
