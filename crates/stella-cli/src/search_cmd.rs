//! `stella search` — describe what you are looking for and get back the
//! files that answer it, ranked by meaning when an embedder is configured.
//!
//! The command runs [`engine::report`], the search ladder over the workspace
//! code graph ([`codegraph`]): exact symbol lookup, semantic embedding rank,
//! graph-name matching, and an index-free file scan, in that order. The text
//! format prints the rendered prose; `--format json` also carries the
//! decided answer — `hits` in rank order, the strategy ladder that ran, the
//! degradation note — so a script probing ranking quality asserts on data
//! instead of regexing prose.

use std::path::Path;

use serde::Serialize;

use crate::query_format::{QueryFormat, Versioned};

pub(crate) mod codegraph;
pub(crate) mod engine;
pub(crate) mod enrich;
pub(crate) mod scan;
pub(crate) mod semantic;

/// One `stella search` answer, machine-readable — the `--format json`
/// envelope. `content` carries the exact text the agent would read; `error`
/// is `None` on success. Every field is always present so a script parsing
/// this need not branch on which key exists. Field order is a courtesy, not
/// the contract (consumers read by key): append new fields, don't reorder.
#[derive(Debug, Clone, Serialize)]
struct SearchAnswer {
    query: String,
    ok: bool,
    /// The strategies that ran, in order, as the `via:` line prints them.
    strategies: Vec<String>,
    /// Why the semantic rung did not run or ran degraded, verbatim.
    note: Option<String>,
    /// The ranked hits, best first — the assertable half of the answer.
    hits: Vec<SearchAnswerHit>,
    content: String,
    error: Option<String>,
}

/// One ranked hit of the JSON envelope, in rank order.
#[derive(Debug, Clone, Serialize)]
struct SearchAnswerHit {
    path: String,
    why: String,
}

/// Entry point for `stella search <query>`.
pub async fn run_search(query: &str, format: QueryFormat) -> Result<(), String> {
    let root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let answer = search_answer_in(&root, query).await;
    render(answer, format)
}

/// The search for a workspace — [`run_search`] minus the cwd resolution and
/// the printing, so tests can drive it against a temp root (the
/// `memory_cmd::list_rows` discipline: no process-global cwd mutation).
async fn search_answer_in(root: &Path, query: &str) -> SearchAnswer {
    let report = engine::report(root, query, engine::SearchConfig::from_env()).await;

    let (ok, content, error) = match report.rendered {
        stella_protocol::tool::ToolOutput::Ok { content } => (true, content, None),
        stella_protocol::tool::ToolOutput::Error { message, .. } => {
            (false, String::new(), Some(message))
        }
    };
    SearchAnswer {
        query: query.trim().to_string(),
        ok,
        strategies: report.strategies.iter().map(ToString::to_string).collect(),
        note: report.note,
        hits: report
            .hits
            .into_iter()
            .map(|hit| SearchAnswerHit {
                path: hit.path,
                why: hit.why,
            })
            .collect(),
        content,
        error,
    }
}

/// Print one answer in the requested format. Fails with the search's own
/// error after printing — under `--format json` the envelope still reaches
/// stdout (`ok: false`, `error` set), and the non-zero exit is the signal a
/// script checks without parsing the body.
fn render(answer: SearchAnswer, format: QueryFormat) -> Result<(), String> {
    match format {
        QueryFormat::Text => {
            if let Some(message) = answer.error {
                return Err(message);
            }
            println!("{}", answer.content);
        }
        QueryFormat::Json => {
            let error = answer.error.clone();
            println!(
                "{}",
                serde_json::to_string_pretty(&Versioned::new(answer))
                    .map_err(|e| format!("serialize: {e}"))?
            );
            if let Some(message) = error {
                return Err(message);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The blank-query refusal, through the same [`engine::report`] path the
    /// command runs. Environment-free: the refusal short-circuits before the
    /// graph is opened or an embedder is resolved, so this asserts the real
    /// plumbing without touching the network.
    #[tokio::test]
    async fn a_blank_query_is_refused_before_anything_is_opened() {
        let dir = tempfile::TempDir::new().unwrap();
        let answer = search_answer_in(dir.path(), "   ").await;
        assert!(!answer.ok);
        let error = answer.error.as_deref().expect("a refusal message");
        assert!(error.contains("`query` is required"), "{error}");
        assert!(answer.hits.is_empty());
        assert!(answer.strategies.is_empty());
        assert!(
            !dir.path().join(".stella").exists(),
            "a refused query must not create workspace state as a side effect"
        );
    }

    /// Both formats surface the same underlying failure — the JSON path must
    /// never swallow it into a zero exit.
    #[test]
    fn render_fails_with_the_answers_own_error_in_both_formats() {
        let failed = |format| {
            render(
                SearchAnswer {
                    query: "q".into(),
                    ok: false,
                    strategies: Vec::new(),
                    note: None,
                    hits: Vec::new(),
                    content: String::new(),
                    error: Some("it broke".into()),
                },
                format,
            )
        };
        assert_eq!(failed(QueryFormat::Text).unwrap_err(), "it broke");
        assert_eq!(failed(QueryFormat::Json).unwrap_err(), "it broke");
    }

    /// The envelope's shape is the contract a test script writes against:
    /// `hits` in rank order with `path`/`why`, the strategy labels, and the
    /// version leading the object.
    #[test]
    fn the_json_envelope_carries_ranked_hits_as_data() {
        let answer = SearchAnswer {
            query: "where is the budget guard".into(),
            ok: true,
            strategies: vec!["semantic (embedding rank over the code-graph index)".into()],
            note: None,
            hits: vec![
                SearchAnswerHit {
                    path: "crates/stella-cli/src/agent/budget.rs".into(),
                    why: "ranked by MEANING (cosine 0.631)".into(),
                },
                SearchAnswerHit {
                    path: "crates/stella-core/src/goal.rs".into(),
                    why: "ranked by MEANING (cosine 0.454)".into(),
                },
            ],
            content: "rendered".into(),
            error: None,
        };
        let value: serde_json::Value =
            serde_json::to_value(Versioned::new(answer)).expect("serializes");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["ok"], true);
        assert_eq!(
            value["hits"][0]["path"], "crates/stella-cli/src/agent/budget.rs",
            "hit order is rank order"
        );
        assert_eq!(value["hits"][1]["path"], "crates/stella-core/src/goal.rs");
        assert!(value["hits"][0]["why"].as_str().unwrap().contains("cosine"));
        assert!(
            value["strategies"][0]
                .as_str()
                .unwrap()
                .starts_with("semantic"),
        );
    }
}
