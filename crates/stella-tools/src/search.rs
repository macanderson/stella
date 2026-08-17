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

use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::registry::Tool;

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
}

impl Search {
    /// The tool as a session registers it: configuration read from the
    /// environment once, at construction.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            config: SearchConfig::from_env(),
        }
    }

    /// The tool with an explicit configuration — for tests, and for a host
    /// that resolves depth and budget itself rather than from the ambient
    /// environment.
    #[must_use]
    pub fn with_config(config: SearchConfig) -> Self {
        Self { config }
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
        search_in(ctx.root(), query, self.config).await
    }
}

/// The tool's answer for one workspace — the seam the tool and the CLI command
/// share, and what a test drives against a temp root.
pub async fn search_in(root: &Path, query: &str, config: SearchConfig) -> ToolOutput {
    engine::report(root, query, config).await.rendered
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
