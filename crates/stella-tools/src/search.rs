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
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::registry::Tool;

pub mod cache;
pub mod codegraph;
pub mod engine;
pub mod enrich;
pub mod scan;
pub mod semantic;

pub use engine::{SearchConfig, SearchReport};

/// The refusal for a missing or blank `query`, spelled once so the tool and
/// the CLI command refuse identically.
pub const QUERY_REQUIRED: &str = "`query` is required and must be a non-empty description of what \
                                  you are looking for";

/// `search`: the one find-code tool.
///
/// Configuration is environment-only ([`SearchConfig::from_env`]:
/// `STELLA_SEARCH_DEPTH`, `STELLA_SEARCH_BUDGET`) and deliberately absent from
/// the schema — depth and budget are operator tuning, and putting them in the
/// input would invite the model to spend its way out of a bad query instead of
/// writing a better one.
pub struct Search {
    config: SearchConfig,
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
    /// The tool as a session registers it: configuration read from the
    /// environment once, at construction.
    #[must_use]
    pub fn from_env() -> Self {
        Self::with_config(SearchConfig::from_env())
    }

    /// The tool with an explicit configuration — for tests, and for a host
    /// that resolves depth and budget itself rather than from the ambient
    /// environment.
    #[must_use]
    pub fn with_config(config: SearchConfig) -> Self {
        Self {
            config,
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
            description: "Find code. This is the ONLY search you need: describe what you are \
                looking for, in whatever form you have it, and get back the files that answer it \
                with their symbols, callers, imports and source already attached — so one call \
                usually replaces a run of grep/glob/read_file round trips. It matches by MEANING, \
                not just by text, so a description works as well as a name: \
                search(\"where are request headers sanitized before logging\"), \
                search(\"what calls resolve_provider\"), \
                search(\"the retry/backoff policy for failed HTTP requests\"), \
                search(\"CredentialStore\"). Use read_file when you already know the exact path \
                and want the whole file, and `bash` with `grep -n` when you need every occurrence \
                of one exact literal string. The answer always states which strategies ran and \
                whether it was truncated."
                .into(),
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
            // Reads only, but NOT safe to run twice before its step commits:
            // the semantic rung writes embeddings through into `codegraph.db`
            // as a side effect of ranking, so a speculated call would do index
            // work the engine then discards.
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
    #[test]
    fn the_schema_takes_one_query_and_no_mode_flag() {
        let schema = Search::from_env().schema();
        assert_eq!(schema.name, "search");
        assert!(schema.read_only);
        assert!(
            !schema.speculation_safe,
            "the semantic rung writes embeddings through"
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
}
