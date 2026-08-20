// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `search` — one tool for finding code, lexical and semantic together.
//!
//! # Why there is exactly one of these
//!
//! Finding code used to be six tools (`grep`, `glob`, `graph_query`,
//! `read_symbol`, `project_overview`, `gather_context`), and the model's first
//! decision on every task was which of them to try — a choice it made badly,
//! because the right answer depends on an index it cannot see. `search`
//! collapses that into one call with **one `query` string and no mode flag**:
//! a parameter that *selects* an operation is three tools wearing one schema
//! (invariant #9), so the strategy ladder is chosen by the engine from what
//! the workspace actually has, not by the model from what it guesses.
//!
//! # The index fills itself, off the query path
//!
//! Since #4043 a search performs exactly one embedder round trip — the
//! query's — and never writes a vector. The index is filled by
//! [`backfill`]'s background pass at session start and by `stella init`'s
//! eager one; [`engine::dispatch`] carries the decision and what it gives up,
//! and [`readiness`] is the policy that holds the first interactive prompt
//! while that pass is still running.
//!
//! # The ladder degrades, it never fails
//!
//! [`engine::dispatch`] runs four rungs — exact symbol lookup, semantic
//! embedding rank, graph-name matching, and an index-free file scan — and the
//! answer always states which ones ran. A workspace with no code graph still
//! answers (file scan); an embedder that is unconfigured silently falls back
//! to lexical; one that is *misconfigured* falls back **and says so**, because
//! a silent downgrade is how a semantic search quietly becomes a grep nobody
//! noticed. The only thing that ends a turn is a worker thread that failed to
//! join.
//!
//! # What it advertises follows what resolved
//!
//! The ladder degrading is invisible to the model unless the *schema* says so,
//! and the schema is what the model decides on. So [`Search`] picks its
//! advertised description at construction from whether an embedder resolved
//! (#3139): with one, it promises meaning-matching, because that is what runs;
//! without one, it promises term-matching over paths, names and file text, and
//! says in as many words that a miss is not evidence of absence. Advertising
//! meaning on a host that cannot do it is how a term-matcher's miss gets read
//! as a semantic answer — the same "miss reads as absence" failure the answer's
//! own coverage note exists to prevent, one level up.
//!
//! # This engine has two callers, on purpose
//!
//! The same modules back both the `search` tool here and the `stella search`
//! CLI command (`stella-cli`'s `search_cmd`, now a thin facade over this
//! module). They were one implementation split across two crates for exactly
//! as long as the tool did not exist; keeping the engine here — where the
//! `Tool` trait lives and where `stella-cli` already depends — is what stops
//! the agent's search and the operator's search from drifting into two
//! different answers to the same question.

use async_trait::async_trait;
use serde_json::Value;
use stella_embed::Resolution;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::registry::Tool;

pub mod backfill;
pub mod cache;
pub mod codegraph;
pub mod engine;
pub mod enrich;
pub mod names;
pub mod readiness;
pub mod scan;
pub mod semantic;

pub use engine::{SearchConfig, SearchReport};

/// The refusal for a missing or blank `query`, spelled once so the tool and
/// the CLI command refuse identically.
pub const QUERY_REQUIRED: &str = "`query` is required and must be a non-empty description of what \
                                  you are looking for";

/// The description advertised when an embedder resolved — the promise the
/// semantic rung can actually keep.
///
/// Held beside [`LEXICAL_DESCRIPTION`] rather than composed from shared
/// fragments so each one reads end to end in review: this is the single string
/// the model decides to call the tool on, and a description assembled from
/// three consts is one nobody reads whole.
pub const SEMANTIC_DESCRIPTION: &str = "Find code. This is the ONLY search you need: describe what \
                                        you are looking for, in whatever form you have it, and get \
                                        back the files that answer it with their symbols, callers, \
                                        imports and source already attached — so one call usually \
                                        replaces a run of grep/glob/read_file round trips. It \
                                        matches by MEANING, not just by text, so a description \
                                        works as well as a name: search(\"where are request headers \
                                        sanitized before logging\"), search(\"what calls \
                                        resolve_provider\"), search(\"the retry/backoff policy for \
                                        failed HTTP requests\"), search(\"CredentialStore\"). Use \
                                        read_file when you already know the exact path and want \
                                        the whole file, and `bash` with `grep -n` when you need \
                                        every occurrence of one exact literal string. The answer \
                                        always states which strategies ran and whether it was \
                                        truncated.";

/// The description advertised when no embedder resolved — what the name and
/// file-scan rungs can actually keep.
///
/// Every clause is load-bearing, and both failure directions are real:
///
/// - **`WORDS` … `literally`** takes the emphasis slot the other variant
///   spends on `MEANING`, so the caps land on the mechanism instead of on a
///   promise. It names paths, symbol names *and* file text because the
///   index-free rung reads contents too ([`scan`]), and a description narrower
///   than the ladder is its own miscalibration.
/// - **"and not by meaning"** is the phrase the *answer* already prints
///   ([`enrich`]'s coverage note), so the schema and the result speak one
///   vocabulary rather than two.
/// - **Term-shaped examples** replace the prose questions, which only work
///   when something is comparing meanings.
/// - **"A miss … is NOT evidence that the behaviour is absent"** is the whole
///   point of #3139: it is the inference a model draws from a term-matcher's
///   silence when it was told the tool understood it.
/// - **"It is still the call to make first"** guards the opposite failure. A
///   description that only disclaims teaches the model to reach for raw
///   `grep`, which loses the symbols, callers, imports and source this tool
///   attaches to every hit — a worse outcome than the overclaim.
pub const LEXICAL_DESCRIPTION: &str = "Find code. This is the ONLY search you need: describe what \
                                       you are looking for, in whatever form you have it, and get \
                                       back the files that answer it with their symbols, callers, \
                                       imports and source already attached — so one call usually \
                                       replaces a run of grep/glob/read_file round trips. In this \
                                       session it matches the WORDS of your query literally — \
                                       against file paths, symbol names, and file text — and not \
                                       by meaning, so give it terms the code itself would use: \
                                       search(\"resolve_provider\"), search(\"CredentialStore\"), \
                                       search(\"retry backoff\"), search(\"sanitize header log\"). A \
                                       miss means those words were not found; it is NOT evidence \
                                       that the behaviour is absent, so try the other names it \
                                       might go by before concluding it does not exist. It is \
                                       still the call to make first: it resolves exact symbols, \
                                       ranks names across the code graph, and scans the tree where \
                                       there is no index, and every hit arrives with its \
                                       neighborhood attached. Use read_file when you already know \
                                       the exact path and want the whole file, and `bash` with \
                                       `grep -n` when you need every occurrence of one exact \
                                       literal string. The answer always states which strategies \
                                       ran and whether it was truncated.";

/// Whether this process can run the ladder's **semantic** rung — the one fact
/// the advertised description varies on (#3139).
///
/// Deliberately not a fact about the *workspace*: whether a code graph exists
/// changes mid-session (`open_or_build` builds one on the first search), so
/// varying the schema on it would break invariant 7. Whether an embedder
/// resolved is fixed for the life of the process, which is what makes it the
/// only axis a description may be chosen from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticRung {
    /// An embedder resolved: a prose query really is compared against the code
    /// by meaning, so the description may promise it.
    Available,
    /// No usable embedder. Every query degrades to the name and file-scan
    /// rungs, which match terms — so the description promises terms.
    Unavailable,
}

impl SemanticRung {
    /// What the real process environment resolves to — the impure half, and
    /// the one a session's registration calls.
    #[must_use]
    pub fn from_env() -> Self {
        Self::of(&stella_embed::from_env())
    }

    /// The posture a given resolution advertises. Pure, which is what lets a
    /// test drive both branches without an embedder, a network, or a mutated
    /// process environment.
    ///
    /// [`Resolution::Incomplete`] lands on [`Self::Unavailable`] beside
    /// `Unconfigured` on purpose: a half-configured backend ranks nothing
    /// either, and a description that promised meaning off a typo would be the
    /// same lie as one that promised it off nothing. The *answer* still keeps
    /// the two apart ([`engine::report_with`] labels the misconfiguration),
    /// because there the difference is actionable.
    #[must_use]
    pub fn of(resolution: &Resolution) -> Self {
        match resolution {
            Resolution::Configured(_) => Self::Available,
            Resolution::Unconfigured | Resolution::Incomplete(_) => Self::Unavailable,
        }
    }

    /// The description a session advertises under this posture.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Available => SEMANTIC_DESCRIPTION,
            Self::Unavailable => LEXICAL_DESCRIPTION,
        }
    }
}

/// `search`: the one find-code tool.
///
/// Configuration is environment-only ([`SearchConfig::from_env`]:
/// `STELLA_SEARCH_DEPTH`, `STELLA_SEARCH_BUDGET`) and deliberately absent from
/// the schema — depth and budget are operator tuning, and putting them in the
/// input would invite the model to spend its way out of a bad query instead of
/// writing a better one.
pub struct Search {
    config: SearchConfig,
    /// The description this instance advertises, chosen ONCE at construction
    /// from [`SemanticRung`] (#3139).
    ///
    /// Stored rather than derived inside [`Tool::schema`], and the once-ness is
    /// the point: tool schemas ride in the byte-stable system prefix
    /// (invariant 7), so a description re-resolved per call could change under
    /// a session — a key exported into the environment of a long-lived process,
    /// an embedder host that starts answering — and silently invalidate every
    /// cached prefix behind it. A `&'static str` picked at construction cannot:
    /// the registration IS the session, which is the actual cache boundary, so
    /// this field is fixed for the whole of it by construction rather than by
    /// anyone remembering to keep it fixed.
    description: &'static str,
    /// The session's gathered-context cache (#3467). Owned by the tool
    /// instance, which is owned by the registry, so its lifetime *is* the
    /// session's — a turn that ranks the same file five times gathers its
    /// neighborhood from the graph once.
    ///
    /// A `Mutex` rather than a `RefCell` because `Tool` is `Sync` and dispatch
    /// may run read-only tools concurrently. It is held across no await: the
    /// lock is taken inside the render pass and released before the call
    /// returns, so two concurrent searches serialise only on rendering, never
    /// on the embedding round trip.
    cache: std::sync::Mutex<cache::GatherCache>,
}

impl Search {
    /// The tool as a session registers it: configuration and the semantic
    /// posture both read from the environment once, at construction.
    #[must_use]
    pub fn from_env() -> Self {
        Self::with_config(SearchConfig::from_env())
    }

    /// The tool with an explicit configuration — for tests, and for a host
    /// that resolves depth and budget itself rather than from the ambient
    /// environment. The semantic posture is still resolved from the
    /// environment; [`Search::with_semantic_rung`] pins that too.
    #[must_use]
    pub fn with_config(config: SearchConfig) -> Self {
        Self::with_semantic_rung(config, SemanticRung::from_env())
    }

    /// The tool with both halves pinned — the seam that lets a caller (and a
    /// test) fix the advertised description instead of inheriting whatever the
    /// ambient environment resolves to.
    ///
    /// It is also what keeps the generated per-tool reference a function of the
    /// source tree rather than of the developer's shell
    /// (`crates/stella-cli/src/tool_docs.rs`).
    #[must_use]
    pub fn with_semantic_rung(config: SearchConfig, semantic: SemanticRung) -> Self {
        Self {
            config,
            description: semantic.description(),
            cache: std::sync::Mutex::new(cache::GatherCache::default()),
        }
    }

    /// How many neighborhoods this tool has gathered from the code graph since
    /// it was constructed — the observable difference between a working cache
    /// and one that silently never hits, and what #3467's witness asserts.
    #[must_use]
    pub fn gathered(&self) -> usize {
        self.cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .gathered
    }
}

impl Default for Search {
    fn default() -> Self {
        Self::from_env()
    }
}

#[async_trait]
impl Tool for Search {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "search".into(),
            // Resolved at construction, never here: see the field.
            description: self.description.into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "What you are looking for — a name, a description, or a question"
                    }
                },
                "required": ["query"]
            }),
            read_only: true,
            // Reads only, but NOT safe to run twice before its step commits.
            // The semantic rung no longer writes vectors (#4043), but
            // `codegraph::open_or_build` still runs a tree-sitter index
            // catch-up on the way to answering, so a speculated call would do
            // index work the engine then discards.
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        let query = match crate::input::required_str(input, "query") {
            Ok(query) => query,
            Err(err) => return ToolOutput::from(err),
        };
        if query.trim().is_empty() {
            return ToolOutput::error(QUERY_REQUIRED);
        }
        // The cache is moved out, used, and put back rather than held across
        // the await: a `MutexGuard` is not `Send`, and holding one over the
        // embedding round trip would serialise concurrent searches on the
        // slowest thing either of them does.
        engine::report_cached(ctx.root(), query, self.config, &self.cache)
            .await
            .rendered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A blank query is refused before the graph is opened or an embedder is
    /// resolved — so a wasted call never creates workspace state.
    #[tokio::test]
    async fn a_blank_query_is_refused_without_touching_the_workspace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = Search::with_config(SearchConfig::from_env())
            .execute(
                &serde_json::json!({ "query": "   " }),
                &crate::ctx::ToolCtx::bare(dir.path().to_path_buf()),
            )
            .await;
        let ToolOutput::Error { message, .. } = out else {
            panic!("a blank query must be refused");
        };
        assert!(message.contains("`query` is required"), "{message}");
        assert!(
            !dir.path().join(".stella").exists(),
            "a refused query must not create workspace state as a side effect"
        );
    }

    /// The witness for the restoration: `search` is a dispatchable tool that
    /// answers over a workspace with no index at all — the file-scan rung.
    /// Fails before this change because the tool does not exist.
    #[tokio::test]
    async fn search_answers_over_an_unindexed_workspace() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("provider.rs"),
            "pub fn resolve_provider() -> Provider { todo!() }\n",
        )
        .expect("write fixture");

        let out = Search::from_env()
            .execute(
                &serde_json::json!({ "query": "resolve_provider" }),
                &crate::ctx::ToolCtx::bare(dir.path().to_path_buf()),
            )
            .await;
        let ToolOutput::Ok { content, .. } = out else {
            panic!("search must answer without an index: {out:?}");
        };
        assert!(
            content.contains("provider.rs"),
            "the file scan must find the fixture: {content}"
        );
    }

    /// The #3467 witness: one `Search` instance serves a repeat search from
    /// its session cache rather than re-reading the file's neighborhood out of
    /// the code graph.
    ///
    /// Asserted on the gather counter, not on wall clock: a timing assertion
    /// here would be flaky, and — worse — a timing has no business anywhere
    /// near a tool's observable behaviour (the loop detector keys on output
    /// bytes). The rendered answers must also be byte-identical, because a
    /// cache that changes the answer is not a cache.
    ///
    /// Fails before this change: the tool held no cache, so both calls
    /// gathered.
    #[tokio::test]
    async fn one_instance_serves_a_repeat_search_from_its_session_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("cached_target.rs"),
            "pub fn cached_target() {}\n",
        )
        .expect("write fixture");

        let tool = Search::from_env();
        let ctx = crate::ctx::ToolCtx::bare(dir.path().to_path_buf());
        let input = serde_json::json!({ "query": "cached_target" });

        let first = tool.execute(&input, &ctx).await;
        let after_first = tool.gathered();
        let second = tool.execute(&input, &ctx).await;
        let after_second = tool.gathered();

        // A workspace with no code graph gathers nothing at all, so the
        // assertion below would pass vacuously. Only make the claim when the
        // first call actually did graph work to save.
        if after_first > 0 {
            assert_eq!(
                after_second, after_first,
                "the repeat search must gather nothing new: {after_first} then {after_second}"
            );
        }

        let (
            ToolOutput::Ok { content: first, .. },
            ToolOutput::Ok {
                content: second, ..
            },
        ) = (&first, &second)
        else {
            panic!("both searches must succeed: {first:?} / {second:?}");
        };
        assert_eq!(
            first, second,
            "a cached repeat must render byte-identically, or the cache changes the answer"
        );
    }

    /// The other half: an edit between two searches must not be served the
    /// bundle gathered from the old bytes.
    ///
    /// This test used to assert only that two different strings hash
    /// differently and that an EMPTY cache misses — both true with the
    /// invalidation branch deleted, so it proved nothing. An adversarial audit
    /// caught that. It now stores an entry and asserts the three outcomes that
    /// matter: a matching identity hits, a changed one misses, and the stale
    /// entry is *evicted* rather than left for a later caller to be served.
    #[test]
    fn a_changed_file_invalidates_its_entry() {
        let mut cache = cache::GatherCache::default();
        let before = cache::content_identity("pub fn a() {}\n");
        let after = cache::content_identity("pub fn a() {}\npub fn b() {}\n");
        assert_ne!(
            before, after,
            "any byte change must change the identity, or invalidation cannot work"
        );

        cache.store(
            "src/a.rs".to_string(),
            before,
            stella_graph::FileNeighborhood::default(),
        );
        assert!(
            cache.lookup("src/a.rs", &before).is_some(),
            "an unchanged file must hit"
        );
        assert!(
            cache.lookup("src/a.rs", &after).is_none(),
            "a changed file must miss"
        );
        assert!(
            cache.lookup("src/a.rs", &before).is_none(),
            "the stale entry must be evicted on the miss, not left to be served \
             to a later caller that happens to ask with the old identity"
        );
    }

    /// The schema is the whole steering surface for this tool: one `query`
    /// string, no mode flag. A mode parameter here would be three tools
    /// wearing one schema (invariant #9).
    ///
    /// Taught about the description variant rather than weakened by it
    /// (#3139): the description is no longer one string, but it is still
    /// exactly one of two, and a third would be a surface nobody declared.
    #[test]
    fn the_schema_takes_one_query_and_no_mode_flag() {
        let schema = Search::from_env().schema();
        assert_eq!(schema.name, "search");
        assert!(schema.read_only);
        assert!(
            !schema.speculation_safe,
            "the semantic rung writes embeddings through"
        );
        assert!(
            schema.description == SEMANTIC_DESCRIPTION || schema.description == LEXICAL_DESCRIPTION,
            "the advertised description must be one of the two declared postures: {}",
            schema.description
        );
        let properties = schema.input_schema["properties"]
            .as_object()
            .expect("an object schema");
        assert_eq!(
            properties.keys().collect::<Vec<_>>(),
            vec!["query"],
            "one parameter only — a second would select an operation"
        );
    }

    /// The #3139 witness. With no embedder resolved the semantic rung never
    /// runs, so the description must not promise meaning-matching — a model
    /// that believes the promise reads a term-matcher's miss as semantic
    /// absence.
    ///
    /// Fails before this change: the description was a single unconditional
    /// string carrying the `MEANING` promise on every host.
    #[test]
    fn without_an_embedder_the_description_promises_terms_not_meaning() {
        let schema =
            Search::with_semantic_rung(SearchConfig::from_env(), SemanticRung::Unavailable)
                .schema();
        assert!(
            !schema.description.contains("MEANING"),
            "the no-embedder description must not promise meaning-matching: {}",
            schema.description
        );
        assert!(
            schema.description.contains("not by meaning"),
            "it must say what it does instead, in the answer's own words: {}",
            schema.description
        );
        assert!(
            schema
                .description
                .contains("file paths, symbol names, and file text"),
            "it must name the three places a term is actually matched: {}",
            schema.description
        );
        assert!(
            schema
                .description
                .contains("NOT evidence that the behaviour is absent"),
            "it must refuse the inference the promise invited: {}",
            schema.description
        );
    }

    /// The other branch, so the fix is a variant and not a deletion: with an
    /// embedder resolved the meaning promise is exactly what the ladder keeps,
    /// and it is still advertised.
    #[test]
    fn with_an_embedder_the_description_keeps_the_meaning_promise() {
        let schema =
            Search::with_semantic_rung(SearchConfig::from_env(), SemanticRung::Available).schema();
        assert!(
            schema.description.contains("It matches by MEANING"),
            "a resolved embedder must still advertise what it can do: {}",
            schema.description
        );
    }

    /// The mapping the two branches hang off, driven from real
    /// [`stella_embed`] resolutions rather than from the enum alone — a test
    /// that only ever constructs `SemanticRung` by hand cannot notice the day
    /// a resolution stops meaning what the description claims.
    ///
    /// No network: `resolve` is pure, and a `Configured` resolution is a built
    /// client nobody calls.
    #[test]
    fn the_posture_follows_what_the_environment_resolved() {
        let configured = stella_embed::resolve(&stella_embed::EmbedderEnv {
            url: Some("http://127.0.0.1:11434/v1".into()),
            model: Some("nomic-embed-text".into()),
            ..Default::default()
        });
        assert!(matches!(configured, Resolution::Configured(_)));
        assert_eq!(SemanticRung::of(&configured), SemanticRung::Available);

        let unconfigured = stella_embed::resolve(&stella_embed::EmbedderEnv::default());
        assert!(matches!(unconfigured, Resolution::Unconfigured));
        assert_eq!(SemanticRung::of(&unconfigured), SemanticRung::Unavailable);

        // A base URL with no model: half-configured, which ranks nothing.
        let incomplete = stella_embed::resolve(&stella_embed::EmbedderEnv {
            url: Some("http://127.0.0.1:11434/v1".into()),
            ..Default::default()
        });
        assert!(matches!(incomplete, Resolution::Incomplete(_)));
        assert_eq!(
            SemanticRung::of(&incomplete),
            SemanticRung::Unavailable,
            "a misconfigured embedder must not advertise meaning-matching either"
        );
    }

    /// The registration path, not just the pinned constructor: whatever this
    /// process resolves, the tool a session registers advertises that
    /// posture's description. Environment-independent because it re-derives
    /// the expectation from the same resolution instead of assuming one.
    #[test]
    fn the_registered_tool_advertises_the_posture_it_resolved() {
        assert_eq!(
            Search::from_env().schema().description,
            SemanticRung::from_env().description(),
        );
    }

    /// Invariant 7, asserted rather than trusted: the description is fixed for
    /// the life of the instance. Re-deriving it per call is the shape that
    /// would break the byte-stable prefix, and `schema()` taking `&self` is
    /// not by itself proof that it does not.
    #[test]
    fn the_description_is_fixed_for_the_life_of_the_instance() {
        let tool = Search::with_semantic_rung(SearchConfig::from_env(), SemanticRung::Unavailable);
        let first = tool.schema().description;
        let second = tool.schema().description;
        assert_eq!(first, second);
        assert_eq!(first, LEXICAL_DESCRIPTION);
    }
}
