// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `search` — the one way to find code.
//!
//! # The measurement that produced this
//!
//! Stella advertises 72 tool schemas per request — 11,797 tokens, 71% of the
//! opening context — against Claude Code's 25 (#3032). Six of those schemas
//! are ways to find code, and across 408 bench trials and 15,304 model calls
//! the model's usage of them was not a distribution, it was a collapse:
//!
//! ```text
//! bash                 10,835 calls   first reached at call  2.2
//! grep                    120         first reached at call  2.3
//! glob                     83         first reached at call 33.1
//! project_overview         33         first reached at call  8.1
//! read_symbol              23         first reached at call  8.0
//! graph_query               3         first reached at call  9.7
//! gather_context            0
//! semantic_code_search      0
//! ```
//!
//! `gather_context` is the decisive row. Its description already promised
//! exactly what this module does — "one deterministic context sweep (grep
//! patterns + file globs + code-graph symbol lookups + bounded excerpts) in a
//! single call… Prefer this over issuing several separate
//! grep/glob/graph_query/read_file calls" — and it was called **zero** times.
//! The concept did not fail. The name and the shape did: a tool whose value
//! depends on the model assembling four parameter lists correctly is a tool
//! the model must first believe in, and it never did.
//!
//! Six ways to find code is a selection problem. One way is not.
//!
//! # Why there is no mode parameter
//!
//! `search(query, mode="regex"|"semantic"|"symbol")` would satisfy invariant
//! 9's letter and break its purpose: a parameter that *selects* an operation
//! is three tools wearing one schema, and it would reproduce the same
//! selection problem one level down, where it is harder to see. **We choose
//! the strategy, not the model.** That is the point — it is what lets the
//! implementation outlive grep, glob and the graph, and it is what makes the
//! code graph and the embedding index *unconditionally exercised*: today
//! `graph_query` has 3 calls in 408 trials, so we cannot tell whether it is
//! bad or merely unchosen. Behind a dispatcher, every search produces
//! evidence about both.
//!
//! # What one call does
//!
//! 1. **Rank.** Always semantically when an embedder resolves — the query is
//!    embedded and the indexed files ranked against it, reusing the vector
//!    table `stella init` already fills (#3014). The `dispatch` function
//!    below documents the ladder of strategies and what each one costs.
//! 2. **Enrich, budget-driven.** The ranked hits are then spent against a
//!    fixed character allowance, adding structure from the code graph:
//!    symbols, kinds, imports, importers, signatures, docs, callers, callees,
//!    a body excerpt. Which facets appear is decided by
//!    [`stella_core::search`] from *what was found and what is affordable* —
//!    never from an inferred intent, because a wrong intent inference is
//!    unrecoverable in a tool with no mode flag to correct it.
//! 3. **Say what happened.** Which strategies ran, and whether the budget
//!    truncated the answer. An agent that cannot tell a complete answer from
//!    a truncated one stops looking.
//!
//! # The bar is "not assemblable in bash", not "better than grep"
//!
//! The same census shows Stella running grep *through the shell* while a
//! native `grep` tool sits in the menu — 8 shell greps on top of 6 native
//! `grep` calls in one trial, same session, same file. It did not forget the
//! tool. It chose the shell for the identical operation, because bash is the
//! only tool where the model can **compose a verb that does not exist**: 8.9
//! command segments per call, median 6, one call routinely listing a
//! directory, grepping three patterns, listing the tests and labelling each
//! section with an `echo`. Every other tool is a fixed verb somebody else
//! chose. That is why a better fixed verb keeps losing.
//!
//! A fixed shape is therefore not automatically safe from the shell. **If
//! this tool returned a list of `file:line` matches it would lose anyway**,
//! because the model can shape that output with a pipe and cannot shape ours.
//! So the answer is deliberately weighted toward what no pipeline of shell
//! commands produces:
//!
//! - it **leads with the ranking and why each hit ranked** — a meaning score
//!   is not expressible in grep at all;
//! - it carries **graph structure** (signature, callers, callees, importers)
//!   as first-class content, not as an appendix to a match list;
//! - it is **not formatted like `grep -n`**, because that shape invites the
//!   comparison this tool loses.
//!
//! Composability-beating comes second, though, and one number keeps that
//! honest: `apply_edits` is the *more* expressive structured tool and loses
//! 42 calls to `edit_file`'s 372 — while failing 33% of the time against
//! `edit_file`'s 5.9%. Expressiveness does not win if the tool is
//! unreliable. Every strategy below is total, every failure degrades to the
//! next rung with its reason quoted, and the answer always states what it
//! did — correct and predictable first.
//!
//! The metric that decides whether this worked is **not** `search`'s call
//! count. It is the count of `bash` calls whose command text is itself a
//! search (`grep`/`rg`/`find` inside a shell invocation). If that does not
//! fall, the fused-tool thesis is wrong in a specific and useful way.
//!
//! # Nothing is replaced
//!
//! `grep`, `glob`, `graph_query`, `read_symbol`, `project_overview` and
//! `gather_context` all keep working exactly as they did. The experiment
//! (#3032) is about what is *sent* in the schema menu, not about what exists.

use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;
use stella_core::search::{Allocation, Depth, allocate};
use stella_embed::{Embedder, Resolution, SimilarityPosture};
use stella_graph::CodeGraph;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::registry::Tool;

pub(crate) mod enrich;
pub(crate) mod scan;

#[cfg(test)]
mod tests;

/// The most hits any one strategy will consider, as a **memory and latency
/// guard — not as the shape of the answer**.
///
/// What is relevant is decided by [`stella_embed::rank::relevant_prefix`] plus
/// the admission floor, and how much of it fits is decided by the budget. A
/// count decides neither, because a count is wrong in both directions at once:
/// a query whose answer genuinely spans forty chunks was truncated at ten with
/// no way for the caller to tell it had been, and a query with two real hits
/// returned ten, where the eight passengers read as findings.
///
/// It stays as a ceiling only because ranking is a full scan over every stored
/// vector, and a pathological query against a monorepo should not materialise
/// a hundred thousand scored rows to then throw nearly all of them away.
/// Reaching it is reported, never silent.
const RANK_CEILING: usize = 200;

/// The most hits the two index-free strategies return. They have no score to
/// threshold on — a name match either matched or did not — so a count is the
/// only bound available to them, and the honest one.
const MAX_NAME_HITS: usize = 10;

/// Files embedded per backend request — one request stays inside every
/// documented body limit while still amortising the round trip.
const EMBED_BATCH: usize = 32;

/// The depth dial, read from configuration at construction.
///
/// **Not a tool argument, deliberately.** Everything measured says a knob the
/// model must learn to use well will not be used well; `gather_context` is
/// the proof. Keeping it out of the input schema also keeps the schema
/// byte-identical across depth settings, so a depth sweep costs no prompt
/// cache (invariant 7).
pub const DEPTH_ENV: &str = "STELLA_SEARCH_DEPTH";

/// The context allowance one answer may spend, in characters.
pub const BUDGET_ENV: &str = "STELLA_SEARCH_BUDGET";

/// Roughly 2,250 tokens at four characters each: large enough that the top
/// hit can carry a body excerpt, small enough that a search is still cheaper
/// than the read it saves.
const DEFAULT_BUDGET_CHARS: usize = 9_000;

/// How the ranked hits were produced. Reported in every answer, because a
/// name match wearing a semantic answer's clothes is worse than no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Strategy {
    /// The query named an indexed symbol exactly, and the graph knows where it
    /// is defined. Not a ranking — a lookup that either hit or did not.
    ///
    /// Not a ladder rung either: it contributes *alongside* the rung that
    /// ranks, because an exact answer must never lose to a guessed one.
    ExactSymbol,
    /// The query and the files were embedded and compared by meaning.
    Semantic,
    /// The code graph's paths and symbol names were matched by term.
    GraphNames,
    /// The workspace was walked and files matched by path and content term —
    /// the only strategy that needs no index at all.
    FileScan,
}

impl Strategy {
    /// The word that appears in the answer's `via:` line.
    const fn label(self) -> &'static str {
        match self {
            Strategy::ExactSymbol => "exact symbol name (code-graph definition sites)",
            Strategy::Semantic => "semantic (embedding rank over the code-graph index)",
            Strategy::GraphNames => "names (path + symbol-name terms over the code-graph index)",
            Strategy::FileScan => "scan (path + content terms over the working tree, no index)",
        }
    }
}

/// One ranked file on its way to the renderer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Hit {
    /// Workspace-relative, forward-slash.
    pub(crate) path: String,
    /// **Why this file ranked**, as a phrase — not a score column.
    ///
    /// A bare number invites the reader to compare it against the next
    /// strategy's number, and the three strategies do not share a scale. It
    /// is also the one part of the answer a shell pipeline cannot produce, so
    /// it is stated in words and shown first (see the module docs).
    pub(crate) why: String,
    /// The symbol that matched, when the ranking knows one — a chunk hit
    /// does, a file hit does not. The renderer leads its detailed facets
    /// (signature, doc, callers, body) with this symbol instead of whatever
    /// happens to sit first in the file: quoting the body of a one-line
    /// struct because it appears above the function the query is actually
    /// about wastes the answer's most expensive facet.
    pub(crate) focus: Option<String>,
}

/// Everything one search decided, before it is rendered.
#[derive(Debug, Clone)]
pub(crate) struct Answer {
    pub(crate) hits: Vec<Hit>,
    /// In the order they ran. [`Strategy::ExactSymbol`] contributes alongside
    /// the rung that ranked; between the ranking rungs, more than one means
    /// an earlier one came back empty or unavailable and the next was tried.
    pub(crate) strategies: Vec<Strategy>,
    /// Why the semantic strategy did not run, or ran degraded — shown
    /// verbatim, never summarised away.
    pub(crate) note: Option<String>,
}

/// The depth and budget one `search` instance runs at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchConfig {
    /// How much to say about the top hit; the tail decays from here.
    pub depth: Depth,
    /// The hard stop, in characters.
    pub budget: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            depth: Depth::DEFAULT,
            budget: DEFAULT_BUDGET_CHARS,
        }
    }
}

impl SearchConfig {
    /// Read the dial from the environment, falling back to the defaults.
    ///
    /// Resolved **once, at registration** rather than per call: a schema and
    /// an answer shape that could change mid-session would make two calls in
    /// one transcript incomparable, and would put a nondeterministic value
    /// upstream of the prompt (invariant 7).
    #[must_use]
    pub fn from_env() -> Self {
        let default = Self::default();
        Self {
            depth: std::env::var(DEPTH_ENV)
                .ok()
                .and_then(|raw| raw.trim().parse::<u8>().ok())
                .map_or(default.depth, Depth::new),
            budget: std::env::var(BUDGET_ENV)
                .ok()
                .and_then(|raw| raw.trim().parse::<usize>().ok())
                .filter(|budget| *budget > 0)
                .unwrap_or(default.budget),
        }
    }
}

/// The one way to find code.
#[derive(Debug, Clone, Copy, Default)]
pub struct Search {
    config: SearchConfig,
}

impl Search {
    /// A `search` running at an explicit depth and budget.
    #[must_use]
    pub fn with_config(config: SearchConfig) -> Self {
        Self { config }
    }

    /// A `search` running at the configured depth ([`SearchConfig::from_env`]).
    #[must_use]
    pub fn from_env() -> Self {
        Self::with_config(SearchConfig::from_env())
    }
}

#[async_trait]
impl Tool for Search {
    /// The description is the entire user interface: the model calls this
    /// tool or does not, on these words alone, and `gather_context` proves
    /// what a bad one costs — a strictly better capability, called zero times
    /// in 408 trials.
    ///
    /// So it is written for the **moment of need** rather than for the
    /// mechanism, and it corrects the name: `search` reads as "lexical
    /// match", and a model that believes that will keep grepping for the
    /// spellings it can guess. The worked argument shapes are load-bearing
    /// for the same reason — they show that a sentence and a symbol name are
    /// both valid input, which no amount of prose asserts as convincingly.
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "search".into(),
            description: "Find code. This is the ONLY search you need: describe what you are \
                          looking for, in whatever form you have it, and get back the files \
                          that answer it with their symbols, callers, imports and source \
                          already attached — so one call usually replaces a run of \
                          grep/glob/read_file round trips.\n\
                          \n\
                          It matches by MEANING, not just by text, so the words you use do not \
                          have to appear anywhere in the code. Ask it the question you actually \
                          have:\n\
                          - `search(\"where are request headers sanitized before logging\")` — \
                          finds the file even if it is called `hkey.rs` and says `scrub`\n\
                          - `search(\"what calls resolve_provider\")` — a symbol name works \
                          just as well as a sentence\n\
                          - `search(\"the retry/backoff policy for failed HTTP requests\")` — \
                          describe behaviour when you cannot name it\n\
                          - `search(\"CredentialStore\")` — a bare type name finds its \
                          definition and its neighbourhood\n\
                          \n\
                          Reach for it the moment you are about to guess a spelling. An agent \
                          grepping `redact|scrub|sanitize|mask` in turn is describing something \
                          semantically to an index that only matches substrings; the spelling \
                          you did not think of is the one grep silently misses, and this call \
                          replaces that whole run. Use `read_file` afterwards when you need \
                          more of a file than the answer carried, and `bash` with `grep -n` \
                          when you need every occurrence of one exact literal string. The \
                          answer always states which strategies ran and whether it was \
                          truncated."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "What you are looking for — a question, a description \
                                        of the behaviour, or a symbol/file name. Plain words \
                                        work; you do not need to guess how the code spells it."
                    }
                },
                "required": ["query"]
            }),
            read_only: true,
            // Embedding is a network write-through: the pass stores vectors
            // in codegraph.db on the way to answering, and the graph open may
            // bootstrap the index besides. A read that writes (#923).
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, root: &Path) -> ToolOutput {
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        // Validation (the blank-query refusal included) lives in [`report`],
        // so this door and the CLI's `stella search` cannot drift apart.
        search(root, query, self.config).await
    }
}

/// The refusal both doors return for a blank query. One string, one place:
/// the tool's `execute` and the CLI's `stella search` reach it through the
/// same [`report`] path, so the wording cannot fork between them.
const QUERY_REQUIRED: &str = "`query` is required: what you are looking for, as a question, a \
                              description of the behaviour, or a symbol name — e.g. \"where \
                              request headers are sanitized\" or \"CredentialStore\"";

/// One ranked hit, as data — the crate-private `Hit` with its fields public,
/// for consumers outside this crate ([`SearchReport`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// Workspace-relative, forward-slash.
    pub path: String,
    /// Why this file ranked, as a phrase — see `Hit::why` for why it is
    /// words rather than a score column.
    pub why: String,
}

/// One search, as data plus the rendered answer — the seam the CLI's
/// `stella search --format json` consumes.
///
/// The agent's door deliberately gets **only** [`Self::rendered`]: the module
/// docs argue at length that a structured match list invites the model to
/// reshape it in a shell pipeline and lose the ranking's meaning. A test
/// script is the opposite consumer — it must assert on hit *order* without
/// parsing prose — so the decided answer rides beside the rendered one
/// instead of being thrown away at render time. Same dispatch, same render,
/// one mechanism; only what survives the call differs.
#[derive(Debug, Clone)]
pub struct SearchReport {
    /// The ranked hits, best first — empty when the search failed.
    pub hits: Vec<SearchHit>,
    /// The strategies that ran, in order, as the labels the answer's `via:`
    /// line prints. More than one means an earlier rung came back empty.
    pub strategies: Vec<&'static str>,
    /// Why the semantic strategy did not run, or ran degraded — verbatim.
    pub note: Option<String>,
    /// Exactly what the agent's door would have returned.
    pub rendered: ToolOutput,
}

impl SearchReport {
    /// The structured half of an [`Answer`], paired with its rendering.
    fn of(answer: &Answer, rendered: ToolOutput) -> Self {
        Self {
            hits: answer
                .hits
                .iter()
                .map(|hit| SearchHit {
                    path: hit.path.clone(),
                    why: hit.why.clone(),
                })
                .collect(),
            strategies: answer.strategies.iter().map(|s| s.label()).collect(),
            note: answer.note.clone(),
            rendered,
        }
    }

    /// A search that never reached dispatch: no hits, no strategies, just
    /// the error the caller must surface.
    fn failed(message: &str) -> Self {
        Self {
            hits: Vec::new(),
            strategies: Vec::new(),
            note: None,
            rendered: ToolOutput::Error {
                message: message.into(),
            },
        }
    }
}

/// One `search` end to end, rendered — the agent's door.
pub(crate) async fn search(root: &Path, query: &str, config: SearchConfig) -> ToolOutput {
    report(root, query, config).await.rendered
}

/// One `search` end to end, as a [`SearchReport`] — the CLI's door
/// (`stella search --format json`), and the single mechanism the tool's own
/// `search` wraps, so a scripted test and an agent call exercise identical
/// code.
///
/// `open_or_build` runs a full `index_all` catch-up pass, which its contract
/// says an async caller must wrap in `spawn_blocking`. The handle is then held
/// across the embedding awaits — the remaining graph calls are single SQLite
/// reads against a local file, so paying a thread hop for each would cost more
/// than it saves. This is `semantic_code_search`'s discipline, deliberately
/// unchanged (`crate::graph::semantic`).
pub async fn report(root: &Path, query: &str, config: SearchConfig) -> SearchReport {
    let query = query.trim();
    if query.is_empty() {
        return SearchReport::failed(QUERY_REQUIRED);
    }

    let owned_root = root.to_path_buf();
    let opened =
        tokio::task::spawn_blocking(move || crate::graph::open_or_build(&owned_root)).await;
    let (graph, index_warning) = match opened {
        Ok(Ok(opened)) => (Some(opened.graph), opened.index_warning),
        // A graph that will not open is a degradation, not a turn ending: the
        // file scan needs no index and is exactly the strategy for a
        // workspace the indexer cannot serve.
        Ok(Err(message)) => (None, Some(message)),
        Err(_) => {
            return SearchReport::failed("the search was cancelled");
        }
    };

    let embedder = match stella_embed::from_env() {
        Resolution::Configured(embedder) => Ok(embedder as Box<dyn Embedder>),
        Resolution::Unconfigured => Err(None),
        // A half-configured backend is neither "off" nor "working". Saying so
        // is why resolution has three outcomes: a typo in a model name must
        // not read as a deliberate lexical setup.
        Resolution::Incomplete(reason) => {
            Err(Some(format!("semantic search is misconfigured: {reason}")))
        }
    };

    let answer = match (graph.as_ref(), embedder) {
        (graph, Ok(embedder)) => dispatch(graph, root, query, Some(embedder.as_ref())).await,
        (graph, Err(note)) => {
            let mut answer = dispatch(graph, root, query, None).await;
            if let Some(note) = note {
                answer.note = Some(note);
            }
            answer
        }
    };

    let output = render(graph.as_ref(), root, query, &answer, config);
    if let Some(graph) = graph {
        graph.shutdown();
    }
    SearchReport::of(
        &answer,
        crate::graph::with_index_warning(output, index_warning),
    )
}

/// Choose and run the ranking strategies, in cost order, stopping at the
/// first that produces hits.
///
/// The ladder, and why it is a ladder rather than a fusion:
///
/// - **Exact symbol** first, and it is not a ranking at all — it is the graph
///   answering "where is this defined" with a fact. It leads whenever it hits.
/// - **Semantic** next whenever an embedder resolves, so the vector index is
///   exercised on every search rather than only when a model happens to pick
///   the semantic tool (#2995). A backend that fails mid-flight degrades to
///   the next rung with the failure quoted, never a lost turn.
/// - **Graph names** next: it searches *symbol* names, which a path glob
///   cannot, so it is genuinely better than nothing — and it is honest about
///   being a name match.
/// - **File scan** last, and it is not an edge case. `stella init` is
///   best-effort by design and a workspace with no tree-sitter grammar for
///   its language is the normal case on Terminal-Bench, where the graph is
///   empty and both rungs above return nothing at all.
///
/// Fusing them instead would mix incomparable scales into one ordering and
/// make the answer's `via:` line a lie.
///
/// # Why the top rung is a stack rather than a stop
///
/// Every rung below the first was unreachable in practice (#3125). "Stop at
/// the first that produces hits" plus an admission floor that rejects nothing —
/// measured: the lowest cosine anywhere in a 158-file corpus was 0.418 against
/// a floor of 0.25, so 158 of 158 files were admitted — means semantic never
/// returns empty and nothing after it ever runs. `via: semantic` on 9 of 9
/// probes. The floor itself is #2993; this is the structural consequence, and
/// it bites hardest exactly where ranking is weakest, on a bare symbol name.
///
/// So the exact rung **prepends** rather than replacing: its hits are
/// certainties and lead, the semantic ranking follows for context with its
/// duplicates removed, and `via:` names both. Concatenating is not the fusion
/// the paragraph above forbids — no score from one rung is ever compared
/// against a score from another, and each hit still says in its own `why:`
/// which kind of answer it is.
pub(crate) async fn dispatch(
    graph: Option<&CodeGraph>,
    root: &Path,
    query: &str,
    embedder: Option<&dyn Embedder>,
) -> Answer {
    let mut strategies = Vec::new();
    let mut note = None;

    let exact = graph.map_or_else(Vec::new, |graph| {
        enrich::exact_symbol_hits(graph, query, MAX_NAME_HITS)
    });
    if !exact.is_empty() {
        strategies.push(Strategy::ExactSymbol);
    }

    if let (Some(graph), Some(embedder)) = (graph, embedder) {
        strategies.push(Strategy::Semantic);
        match semantic_hits(graph, embedder, query).await {
            Ok((hits, coverage)) if !hits.is_empty() => {
                return Answer {
                    hits: lead_with(exact, hits),
                    strategies,
                    // Coverage rides even on a successful answer: it is
                    // exactly the successful-looking partial answer that
                    // needs the caveat.
                    note: coverage,
                };
            }
            // An empty semantic answer over a partial index is the most
            // misleading outcome of all — "nothing matched" reads as absence.
            Ok((_, coverage)) => note = coverage,
            Err(reason) => note = Some(reason),
        }
    }

    // A definition site is a complete answer on its own, so it is returned
    // before the term-matching rungs rather than after them — reaching a name
    // match having already found the definition would rank the certainty below
    // the guesses.
    if !exact.is_empty() {
        return Answer {
            hits: exact,
            strategies,
            note,
        };
    }

    if let Some(graph) = graph {
        strategies.push(Strategy::GraphNames);
        let (hits, matched) = enrich::name_hits(graph, query, MAX_NAME_HITS);
        if !hits.is_empty() {
            if matched > hits.len() {
                note = merge_notes(
                    note,
                    Some(format!(
                        "{matched} files matched by name; showing the {} with the most matched \
                         terms — narrow the query to see the rest",
                        hits.len()
                    )),
                );
            }
            // No pinning here: an exact hit returns above, so this rung only
            // ever runs when the graph knew no symbol by that name.
            return Answer {
                hits,
                strategies,
                note,
            };
        }
    }

    strategies.push(Strategy::FileScan);
    let scanned = scan::scan_hits(root, query, MAX_NAME_HITS);
    if scanned.exhausted {
        note = merge_notes(
            note,
            Some(format!(
                "PARTIAL SCAN — the walk stopped at the first {} files it reached, so a miss \
                 here is inconclusive; run `stella init` to build an index, or use `bash` with \
                 `grep -rn` for an exhaustive pass",
                scan::MAX_FILES_SCANNED
            )),
        );
    }
    if scanned.matched > scanned.hits.len() {
        note = merge_notes(
            note,
            Some(format!(
                "{} files matched by term; showing the {} with the most matched terms — narrow \
                 the query to see the rest",
                scanned.matched,
                scanned.hits.len()
            )),
        );
    }
    Answer {
        hits: scanned.hits,
        strategies,
        note,
    }
}

/// One note out of up to two, oldest first — the `Answer` carries a single
/// caveat line, and two truths must not cost one of them.
fn merge_notes(base: Option<String>, extra: Option<String>) -> Option<String> {
    match (base, extra) {
        (Some(base), Some(extra)) => Some(format!("{base}; {extra}")),
        (base, extra) => base.or(extra),
    }
}

/// Put the certainties first and let the ranking follow, minus anything the
/// certainties already named.
///
/// Order within each group is preserved, and no score from one group is ever
/// weighed against a score from the other — the concatenation is the whole
/// operation. A file reached by both is reported once, as the exact hit,
/// because "this is where it is defined" is strictly more information than
/// "this scored 0.61".
fn lead_with(exact: Vec<Hit>, ranked: Vec<Hit>) -> Vec<Hit> {
    if exact.is_empty() {
        return ranked;
    }
    let led: std::collections::HashSet<String> = exact.iter().map(|hit| hit.path.clone()).collect();
    let mut hits = exact;
    hits.extend(ranked.into_iter().filter(|hit| !led.contains(&hit.path)));
    hits
}

/// Embed what is pending, embed the query, rank. `Err` carries a reason
/// already phrased for the answer's note.
///
/// **Both rungs, merged — not chunks-then-files.** Ranking chunks and
/// returning early whenever they produced anything let a *partially filled*
/// chunk index shadow a better-covered file index, and the failure was total
/// rather than marginal: the chunk pass fills in `ORDER BY path` at
/// `MAX_FILES_PER_CHUNK_PASS` files a call, so on this workspace it held
/// 4,011 of 29,334 chunks, every one under `arenabench/` or `bench/`, with the
/// frontier sitting alphabetically short of `crates/`. Every Rust file was
/// invisible to the sharp rung while the file rung already covered 69% of the
/// tree — so a query about `crates/` got a confident answer assembled entirely
/// from the bench harness.
///
/// Merging is legitimate here and *only* here: chunk and file vectors are
/// written by the same embedder under the same `fingerprint`, so they are one
/// vector space and their cosines are directly comparable. That is not true
/// across the strategy ladder, which is why `dispatch` still refuses to fuse.
///
/// A file reached by both rungs is reported once, keeping whichever hit
/// scored higher — that score is the honest "why it ranked". On a tie the
/// chunk hit wins (the sort is stable and chunks are listed first), which is
/// the right tiebreak: same score meaning, plus the symbol or section that
/// matched, a citation a file vector cannot produce.
async fn semantic_hits(
    graph: &CodeGraph,
    embedder: &dyn Embedder,
    query: &str,
) -> Result<(Vec<Hit>, Option<String>), String> {
    let fingerprint = embedder.fingerprint().id();
    catch_up_embeddings(graph, embedder, &fingerprint).await?;
    catch_up_chunk_embeddings(graph, embedder, &fingerprint).await?;

    let query_vector = match embedder.embed(&[query.to_string()]).await {
        Ok(mut vectors) if !vectors.is_empty() => vectors.remove(0).vector,
        Ok(_) => return Err("the embedder returned no vector for the query".into()),
        Err(error) => return Err(format!("the embedder failed: {error}")),
    };

    // Only a `Semantic` backend has earned a floor. Under `Surface` the score
    // orders and never admits, so nothing is filtered out on its say-so.
    let floor = match embedder.similarity_posture() {
        SimilarityPosture::Semantic { admission_floor } => admission_floor,
        SimilarityPosture::Surface => f32::NEG_INFINITY,
    };

    let chunks = graph
        .rank_chunks_by_vector(&fingerprint, &query_vector, floor, RANK_CEILING)
        .map_err(|error| format!("the chunk index could not be read: {error}"))?;
    let files = graph
        .rank_files_by_vector(&fingerprint, &query_vector, floor, RANK_CEILING)
        .map_err(|error| format!("the vector index could not be read: {error}"))?;

    let note = merge_notes(
        coverage_note(graph, &fingerprint),
        ceiling_note(chunks.len(), files.len()),
    );
    Ok((merge_rungs(&chunks, &files), note))
}

/// The disclosure [`RANK_CEILING`]'s doc promises: a scored list that filled
/// its memory guard may have dropped better-than-nothing candidates past it,
/// and a caller reading a miss as absence must be told so.
fn ceiling_note(chunks: usize, files: usize) -> Option<String> {
    (chunks >= RANK_CEILING || files >= RANK_CEILING).then(|| {
        format!(
            "RANK CEILING — the candidate list filled its {RANK_CEILING}-row guard, so \
             lower-scoring files beyond it were never considered; narrow the query if the file \
             you expected is missing"
        )
    })
}

/// One ordering over both rungs, best first, one hit per file.
///
/// See [`semantic_hits`] for why merging is sound: one embedder, one
/// fingerprint, one vector space.
fn merge_rungs(
    chunks: &[stella_graph::ScoredChunk],
    files: &[stella_embed::rank::Scored],
) -> Vec<Hit> {
    let mut scored: Vec<(f32, String, String, Option<String>)> = chunks
        .iter()
        .map(|chunk| {
            (
                chunk.score,
                chunk.path.clone(),
                format!(
                    "ranked by MEANING against your query — best match is `{}` ({}) at line {} \
                     (cosine {:.3})",
                    chunk.name, chunk.kind, chunk.start_line, chunk.score
                ),
                Some(chunk.name.clone()),
            )
        })
        .chain(files.iter().map(|file| {
            (
                file.score,
                file.key.clone(),
                format!(
                    "ranked by MEANING against your query, over the whole file (cosine {:.3})",
                    file.score
                ),
                None,
            )
        }))
        .collect();
    // Descending by score, then by path, so the ordering is total and the
    // answer is byte-stable across runs (invariant 7).
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });

    // The cut is taken over the merged SCORES, before collapsing to files:
    // the boundary is a property of the score distribution, and collapsing
    // discards scores. The synthetic key keeps `relevant_prefix`'s input
    // unique without reordering it.
    let ordered: Vec<stella_embed::rank::Scored> = scored
        .iter()
        .enumerate()
        .map(|(index, (score, path, _, _))| stella_embed::rank::Scored {
            key: format!("{index:06}#{path}"),
            score: *score,
        })
        .collect();
    let cut = stella_embed::rank::relevant_prefix(
        &ordered,
        stella_embed::rank::DEFAULT_RELEVANCE_GAP_RATIO,
        stella_embed::rank::DEFAULT_MIN_BOUNDARY_GAP,
    );

    let mut seen: Vec<String> = Vec::new();
    let mut hits = Vec::new();
    for (_, path, why, focus) in scored.into_iter().take(cut) {
        if seen.iter().any(|p| p == &path) {
            continue;
        }
        seen.push(path.clone());
        hits.push(Hit { path, why, focus });
    }
    hits
}

/// How much of the workspace this ranking could actually see, when that is
/// less than all of it.
///
/// **An incomplete index reported as a complete one is worse than an empty
/// one**, and this tool had no way to say so: it printed `via: semantic` over
/// a ranking drawn from 14% of the workspace — every one of those chunks in
/// the same two directories — with nothing in the answer to suggest the other
/// 86% had never been looked at. The argument the module already makes about
/// budget truncation, that an agent which cannot tell a complete answer from a
/// partial one stops looking, applies to coverage exactly as it does to
/// length. Coverage was the half that was silent.
///
/// The "not a random sample" sentence is the load-bearing one. Embedding fills
/// in path order, so a partial index is not a uniform thinning of the tree; it
/// is a prefix of it, and a caller who assumes otherwise will read a miss as
/// evidence of absence.
///
/// `None` when both rungs are complete, so a healthy workspace pays no tokens
/// for the disclosure.
///
/// **And a complete workspace must be able to reach `None`** (#3124). This
/// asked `embedded_chunk_count >= symbol_count` until chunk coverage was
/// measured on a corpus small enough to finish: chunk rows are keyed on the
/// hash of the rendered chunk, so two byte-identical bodies in one file store
/// one row for two symbols and that comparison is unsatisfiable forever. A
/// 158-file workspace froze at 1,810 of 1,814 across thirty passes and warned
/// on every search — and the warning's own advice is "use `bash` with `grep -n`
/// for anything exact", which is precisely the call this module exists to stop
/// the model making. A disclosure nobody can ever clear is not a disclosure; it
/// is noise with a suggestion attached.
///
/// Both halves are like-for-like counts of **files**:
/// `pending_chunk_file_count` is exact by construction (see
/// `stella_graph`'s `NEEDS_CHUNK_PASS`), and whole-file vectors are one row per
/// file, so neither side can dedup out from under the other.
///
/// **A counter that cannot be read fails toward disclosure, not toward
/// silence.** `.ok()?` made a read error indistinguishable from full coverage,
/// which is the one direction this note must never fail in: the two hazards it
/// exists for — a partial index that reads as complete, and a complete index
/// that can never say so — are both cases of the answer misrepresenting what it
/// saw, and an unreadable counter is a third.
fn coverage_note(graph: &CodeGraph, fingerprint: &str) -> Option<String> {
    let (Ok(total_files), Ok(files), Ok(chunks_pending)) = (
        graph.file_count(),
        graph.embedded_file_count(fingerprint),
        graph.pending_chunk_file_count(fingerprint),
    ) else {
        return Some(
            "COVERAGE UNKNOWN — the index's coverage counters could not be read, so how much \
             of the workspace this ranking saw cannot be stated; treat a miss as inconclusive"
                .to_string(),
        );
    };
    if chunks_pending == 0 && files >= total_files {
        return None;
    }
    Some(format!(
        "PARTIAL INDEX — this ranking saw {files} of {total_files} files, and {chunks_pending} \
         indexed file(s) still have symbols with no vector. Embedding fills in path order a \
         batch per call, so the remainder is NOT a random sample: it is the tail of the tree \
         alphabetically. Run `stella init` to finish it; until then treat a miss as \
         inconclusive and use `bash` with `grep -n` for anything exact."
    ))
}

/// Embed every indexed file still pending under `fingerprint`, up to the
/// per-pass cap. Shares `stella-graph`'s pending-scan cursor with
/// `semantic_code_search`, so the two tools warm one index rather than two.
async fn catch_up_embeddings(
    graph: &CodeGraph,
    embedder: &dyn Embedder,
    fingerprint: &str,
) -> Result<(), String> {
    let scan = graph
        .files_pending_embedding(fingerprint, stella_graph::MAX_FILES_PER_PASS)
        .map_err(|error| format!("the code graph could not be read: {error}"))?;
    for chunk in scan.files.chunks(EMBED_BATCH) {
        let texts: Vec<String> = chunk.iter().map(|pending| pending.text.clone()).collect();
        let embeddings = embedder
            .embed(&texts)
            .await
            .map_err(|error| format!("the embedder failed: {error}"))?;
        let rows: Vec<stella_graph::FileVector> = chunk
            .iter()
            .zip(embeddings)
            .map(|(pending, embedding)| stella_graph::FileVector {
                path: pending.path.clone(),
                content_sha256: pending.content_sha256.clone(),
                vector: embedding.vector,
            })
            .collect();
        graph
            .store_file_vectors(fingerprint, &rows)
            // Not recoverable by continuing: the next batch would be written
            // into the same broken store and the pass would report success
            // having stored nothing.
            .map_err(|error| format!("the vector index could not be written: {error}"))?;
    }
    Ok(())
}

/// Embed every indexed chunk still pending under `fingerprint`, up to the
/// per-pass cap.
///
/// **One file per store call**, because the store's sweep — the thing that
/// removes a deleted function's vector — keys on the file's content hash, so
/// writing half a file's chunks and then the other half would have the second
/// write delete the first's rows.
///
/// A chunk whose rendered text is unchanged arrives with no `text` and costs
/// no request: it is re-stamped with the file's new hash and keeps its vector.
/// That is what makes editing one function in a 60-symbol file cost one
/// embedding rather than sixty.
async fn catch_up_chunk_embeddings(
    graph: &CodeGraph,
    embedder: &dyn Embedder,
    fingerprint: &str,
) -> Result<(), String> {
    let scan = graph
        .chunks_pending_embedding(fingerprint, stella_graph::MAX_FILES_PER_CHUNK_PASS)
        .map_err(|error| format!("the code graph could not be read: {error}"))?;

    for file in &scan.files {
        embed_and_store_chunk_file(graph, embedder, fingerprint, file).await?;
    }
    Ok(())
}

/// Embed one file's pending chunks and store them — the single place a chunk
/// vector comes into existence, shared by the lazy per-query catch-up above
/// and the eager `stella init` pass below, so there is one render, one table
/// and one fingerprint discipline (mirrors `embed_batch` in
/// `crate::graph::semantic` for whole-file vectors).
async fn embed_and_store_chunk_file(
    graph: &CodeGraph,
    embedder: &dyn Embedder,
    fingerprint: &str,
    file: &stella_graph::PendingChunkFile,
) -> Result<(), String> {
    let mut vectors: Vec<Option<Vec<f32>>> = vec![None; file.chunks.len()];
    let needed: Vec<(usize, &str)> = file
        .chunks
        .iter()
        .enumerate()
        .filter_map(|(index, chunk)| chunk.text.as_deref().map(|text| (index, text)))
        .collect();

    for batch in needed.chunks(EMBED_BATCH) {
        let texts: Vec<String> = batch.iter().map(|(_, text)| (*text).to_string()).collect();
        let embeddings = embedder
            .embed(&texts)
            .await
            .map_err(|error| format!("the embedder failed: {error}"))?;
        for ((index, _), embedding) in batch.iter().zip(embeddings) {
            vectors[*index] = Some(embedding.vector);
        }
    }

    let rows: Vec<stella_graph::ChunkVector> = file
        .chunks
        .iter()
        .zip(vectors)
        .map(|(chunk, vector)| stella_graph::ChunkVector {
            chunk_sha256: chunk.chunk_sha256.clone(),
            name: chunk.name.clone(),
            kind: chunk.kind.clone(),
            start_line: chunk.start_line,
            end_line: chunk.end_line,
            vector,
        })
        .collect();
    graph
        .store_chunk_vectors(fingerprint, &file.path, &file.file_sha256, &rows)
        // Not recoverable by continuing: the next file would be written
        // into the same broken store and the pass would report success
        // having stored nothing.
        .map_err(|error| format!("the chunk index could not be written: {error}"))
        .map(|_| ())
}

/// The most files one eager (`stella init`) chunk-embedding pass will
/// process. Matches `crate::graph::semantic::MAX_FILES_PER_EAGER_PASS`'s cap
/// and its reasoning: this pass runs before any turn starts, so its budget is
/// the user's money and patience rather than a round trip, and a workspace
/// larger than this gets a **stated** partial index rather than a silent one.
pub const MAX_FILES_PER_CHUNK_EAGER_PASS: usize = 2_000;

/// What an eager chunk-embedding pass did, as data the caller renders. Total
/// by construction and never a `Result` — `stella init` must succeed with no
/// embedder, a broken embedder, and a workspace too big to finish in one
/// pass, and telling those apart is something a human reads, not something a
/// caller branches on (same contract as
/// `crate::graph::semantic::WarmOutcome`, kept as its own type because a
/// chunk pass counts files-with-pending-chunk-work, not files-with-no-vector
/// — the two are not interchangeable once a file can be partially chunked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkWarmOutcome {
    /// The pass ran. `files_remaining` is what the cap left for the lazy
    /// per-query pass (`catch_up_chunk_embeddings`) to pick up on the first
    /// `search` call.
    Warmed {
        /// Files whose chunks this pass embedded or re-stamped.
        files_embedded: usize,
        /// Indexed files still carrying a symbol with no chunk vector under
        /// this fingerprint, per the same pre-filter `chunks_pending_embedding`
        /// scans with — an upper bound, not an exact count: a file whose
        /// symbols happen to render byte-identical text to one another can
        /// read as pending forever even once fully covered (see the "no
        /// progress" comment in `warm_chunks_opened` — filed as #3128).
        files_remaining: usize,
        /// How many of those this pass could not read from disk. Separated
        /// from `files_remaining` for the reason
        /// `crate::graph::semantic::WarmOutcome::Warmed::unreadable` gives:
        /// the cap's leftovers embed themselves on the next query, an
        /// unreadable file never will.
        unreadable: usize,
    },
    /// The pass stopped early. `reason` is prose meant to be shown verbatim;
    /// whatever was embedded before the failure is committed and counted.
    Failed {
        /// Files whose chunks were embedded before the failure — each file
        /// commits as it completes.
        files_embedded: usize,
        reason: String,
    },
}

/// Embed the workspace's indexed chunks — symbols, markdown sections —
/// **without answering a query**: the `stella init` pass for `search`'s
/// primary ranking rung (#3098).
///
/// The lazy per-query pass above is right for a long session and wrong for a
/// single-turn run in a fresh checkout: it fires only once `search` has
/// already been called, capped at `stella_graph::MAX_FILES_PER_CHUNK_PASS`
/// (64) files, so a workspace this crate's own repository's size needs
/// roughly 25 calls before its first full pass completes — meaning the first
/// several searches in a session rank against whichever files happened to be
/// processed first, not the best answer. Running the same pass here, ahead of
/// the first turn, makes that work free from the agent's perspective —
/// mirroring `crate::graph::semantic::warm_file_vectors` one layer down, and
/// deliberately not `open_or_build`, for the identical reason it gives: the
/// caller has just run `index_all`, and a second catch-up pass would re-walk
/// and re-hash a tree nothing has touched since.
pub async fn warm_chunk_vectors(
    root: &Path,
    embedder: &dyn Embedder,
    limit: usize,
) -> ChunkWarmOutcome {
    let graph = stella_store::workspace_private_sqlite_path(root, "codegraph.db")
        .map_err(|error| format!("cannot prepare the code graph store: {error}"))
        .and_then(|db_path| {
            CodeGraph::open(root, &db_path)
                .map_err(|error| format!("could not open the code graph: {error}"))
        });
    let graph = match graph {
        Ok(graph) => graph,
        Err(reason) => {
            return ChunkWarmOutcome::Failed {
                files_embedded: 0,
                reason,
            };
        }
    };
    let outcome = warm_chunks_opened(&graph, embedder, limit).await;
    graph.shutdown();
    outcome
}

async fn warm_chunks_opened(
    graph: &CodeGraph,
    embedder: &dyn Embedder,
    limit: usize,
) -> ChunkWarmOutcome {
    let fingerprint = embedder.fingerprint().id();
    let mut files_embedded = 0usize;
    let mut unreadable = 0usize;
    while files_embedded < limit {
        let want = stella_graph::MAX_FILES_PER_CHUNK_PASS.min(limit - files_embedded);
        let scan = match graph.chunks_pending_embedding(&fingerprint, want) {
            Ok(scan) => scan,
            Err(error) => {
                return ChunkWarmOutcome::Failed {
                    files_embedded,
                    reason: format!("cannot read the code graph: {error}"),
                };
            }
        };
        // Overwritten, never summed — same discipline as
        // `crate::graph::semantic::warm_opened`: each round rescans the
        // pending set from the start, so an unreadable file is stepped over
        // again every round and summing would multiply it by the round count.
        unreadable = unreadable.max(scan.unreadable);
        // No files means the pending set is exhausted — a window landing on
        // unreadable rows keeps filling past them, so this is genuinely done
        // rather than a scan that gave up early (#3016).
        if scan.files.is_empty() {
            break;
        }
        // The pre-filter is exact since #3128: it reads a per-file coverage
        // marker rather than comparing a raw symbol count against stored rows,
        // so a file whose symbols render byte-identical text is no longer
        // selected forever after being fully embedded.
        //
        // This guard stays anyway, and is not redundant belt-and-braces. It
        // asserts the loop's own termination condition — a window that embeds
        // nothing cannot be made progress by another window — which holds
        // whatever the pre-filter's precision happens to be. Deleting it would
        // make the loop's boundedness depend on a SQL predicate in another
        // crate being right, which is exactly the coupling that produced
        // #3128. `PendingChunkFile::to_embed()` is the per-symbol truth.
        let mut made_progress = false;
        for file in &scan.files {
            if file.to_embed() > 0 {
                made_progress = true;
            }
            if let Err(reason) =
                embed_and_store_chunk_file(graph, embedder, &fingerprint, file).await
            {
                return ChunkWarmOutcome::Failed {
                    files_embedded,
                    reason,
                };
            }
            files_embedded += 1;
        }
        if !made_progress {
            break;
        }
    }

    let files_remaining = graph.pending_chunk_file_count(&fingerprint).unwrap_or(0);
    ChunkWarmOutcome::Warmed {
        files_embedded,
        files_remaining,
        unreadable,
    }
}

/// Render an answer against the allocation the budget allows.
///
/// The two lines that are never optional are the `via:` line and the
/// truncation line — the first because an agent must be able to tell a
/// meaning match from a name match, the second because an agent that cannot
/// tell a complete answer from a truncated one stops looking.
pub(crate) fn render(
    graph: Option<&CodeGraph>,
    root: &Path,
    query: &str,
    answer: &Answer,
    config: SearchConfig,
) -> ToolOutput {
    let allocation: Allocation = allocate(answer.hits.len(), config.depth, config.budget);

    let mut content = String::new();
    if let Some(note) = &answer.note {
        content.push_str(&format!("({note})\n"));
    }
    content.push_str(&format!(
        "search `{query}` — {} result(s) at depth {}",
        allocation.granted.len(),
        config.depth.level()
    ));
    content.push_str("\nvia: ");
    content.push_str(
        &answer
            .strategies
            .iter()
            .map(|strategy| strategy.label())
            .collect::<Vec<_>>()
            .join(" -> "),
    );

    if answer.hits.is_empty() {
        content.push_str("\nno file matched. Widen the description, or use `bash` with `grep -n` for an exact literal.");
        return ToolOutput::Ok { content };
    }

    for (hit, depth) in answer.hits.iter().zip(&allocation.granted) {
        content.push('\n');
        content.push_str(&enrich::render_hit(graph, root, hit, *depth));
    }

    if allocation.truncated() {
        content.push_str(&format!(
            "\nTRUNCATED: {} further result(s) were dropped to stay inside the context budget \
             ({} of {} characters spent). They exist — narrow the query to see them.",
            allocation.omitted, allocation.spent, config.budget
        ));
    }

    ToolOutput::Ok { content }
}
