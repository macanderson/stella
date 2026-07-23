//! `read_symbol` — read a named symbol's exact source span, resolved through
//! the code graph instead of guessed line offsets (issue #330).
//!
//! `graph_query definitions` answers "where is it" with a citation and a
//! truncated snippet; `read_file` makes the caller guess `offset`/`limit`.
//! This tool closes that round-trip: resolve the symbol to its indexed
//! `(path, start_line, end_line)` span, then read exactly that range through
//! the SAME [`crate::read::ReadFile`] instance registered as `read_file` — so
//! 1-based numbering, the returned-lines cap, and the per-file read tally
//! ("read N× this session") stay consistent across both surfaces. The
//! registry drains [`SpanReadLedger`] after each successful call to land the
//! matching `R` event in the file-touch ledger: the file is resolved
//! mid-execution, so the registry cannot classify it from the input up front
//! the way it does for `read_file`.
//!
//! On multiple definitions the sites are listed (like `graph_query`) and the
//! caller picks one via `path` — never silently the first match.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::registry::Tool;

/// Resolved span-reads awaiting the registry's file-touch drain: the tool
/// pushes each successfully read root-relative path, and the registry takes
/// them once per execution and records one `R` event apiece — the citation
/// ledger's drain discipline, so no read is ever recorded twice.
pub type SpanReadLedger = Arc<Mutex<Vec<String>>>;

pub struct ReadSymbol {
    /// The same instance registered as `read_file`, so both surfaces share
    /// one per-file read tally.
    read_file: Arc<crate::read::ReadFile>,
    span_reads: SpanReadLedger,
}

impl ReadSymbol {
    pub fn new(read_file: Arc<crate::read::ReadFile>, span_reads: SpanReadLedger) -> Self {
        Self {
            read_file,
            span_reads,
        }
    }
}

#[async_trait]
impl Tool for ReadSymbol {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_symbol".into(),
            description: "Read a named symbol's exact source span (function/struct/class body) \
                          resolved through the code graph — no line-offset guessing and no \
                          over-reading. If the name is defined in more than one place the \
                          sites are listed; call again with `path` to pick one."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "The symbol name to read (exact match, as indexed)" },
                    "path": { "type": "string", "description": "Workspace-relative file path that disambiguates when the name is defined in more than one file (optional)" },
                    "reason": { "type": "string", "description": "Why you are reading this symbol — recorded in the session's file-touch audit log" }
                },
                "required": ["name"]
            }),
            read_only: true,
        }
    }

    async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput {
        let name = match input
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            Some(n) => n,
            None => {
                return ToolOutput::Error {
                    message: "missing required field `name`".into(),
                };
            }
        };

        // Same open → query → shutdown discipline (and on-first-use build) as
        // `graph_query` — no held handles across turns.
        let graph = match crate::graph::open_or_build(root) {
            Ok(g) => g,
            Err(message) => return ToolOutput::Error { message },
        };
        let spans = graph.definition_spans(name);
        graph.shutdown();
        let mut spans = match spans {
            Ok(spans) => spans,
            Err(e) => {
                return ToolOutput::Error {
                    message: format!("code-graph lookup failed: {e}"),
                };
            }
        };
        if spans.is_empty() {
            return ToolOutput::Error {
                message: format!(
                    "no definition of `{name}` in the code graph (index may be stale — \
                     `stella init` re-indexes) — try graph_query references, or grep"
                ),
            };
        }

        // Optional `path` disambiguator. The graph stores normalized
        // root-relative forward-slash paths, so the caller's spelling is
        // normalized the same way before comparing.
        if let Some(raw) = input.get("path").and_then(|v| v.as_str()) {
            let wanted = crate::file_touch::normalize_workspace_path(root, raw)
                .unwrap_or_else(|| raw.to_string());
            let in_file: Vec<_> = spans.iter().filter(|s| s.path == wanted).cloned().collect();
            if in_file.is_empty() {
                return ToolOutput::Error {
                    message: format!(
                        "`{name}` has no definition in `{wanted}` — it is defined at:\n{}",
                        listing(&spans)
                    ),
                };
            }
            spans = in_file;
        }

        if spans.len() > 1 {
            // Never silently pick one (issue #330): list the sites like
            // `graph_query` and let the caller choose.
            let one_file = spans.windows(2).all(|w| w[0].path == w[1].path);
            let hint = if one_file {
                "several sites in one file — read_file with the offsets shown reads a specific one"
            } else {
                "call read_symbol again with `path` to pick one"
            };
            return ToolOutput::Ok {
                content: format!(
                    "`{name}` has {} definitions — {hint}:\n{}",
                    spans.len(),
                    listing(&spans)
                ),
            };
        }

        let span = &spans[0];
        let span_lines = span.end_line.saturating_sub(span.start_line) as usize + 1;
        let read = self
            .read_file
            .execute(
                &serde_json::json!({
                    "path": span.path,
                    "offset": span.start_line,
                    "limit": span_lines,
                }),
                root,
            )
            .await;
        match read {
            ToolOutput::Ok { content } => {
                self.span_reads
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(span.path.clone());
                let mut out = format!("{}\n{content}", citation(span));
                if span_lines > crate::read::MAX_LINES {
                    out.push_str(&format!(
                        "\n(note: the span is {span_lines} lines and only the first {} are \
                         shown — read_file offset={} continues it)",
                        crate::read::MAX_LINES,
                        span.start_line as usize + crate::read::MAX_LINES
                    ));
                }
                ToolOutput::Ok { content: out }
            }
            // The graph answered but the file didn't (deleted or unreadable
            // since indexing) — surface read_file's own named error.
            err => err,
        }
    }
}

/// `fn name (path:start-end)` — the L-C4 human citation shape with the full
/// 1-based inclusive span appended.
fn citation(span: &stella_graph::SymbolSpan) -> String {
    format!(
        "{} {} ({}:{}-{})",
        span.kind, span.name, span.path, span.start_line, span.end_line
    )
}

fn listing(spans: &[stella_graph::SymbolSpan]) -> String {
    spans
        .iter()
        .map(|s| format!("- {}", citation(s)))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `target_fn` occupies exactly lines 5–8; `alpha`/`omega` bracket it so
    /// an off-by-one or an over-read is visible in the assertion.
    const FIXTURE: &str = "fn alpha() {\n    let a = 1;\n}\n\nfn target_fn() {\n    let x = 1;\n    let y = 2;\n}\n\nfn omega() {}\n";

    fn tool() -> ReadSymbol {
        ReadSymbol::new(Arc::new(crate::read::ReadFile::default()), Arc::default())
    }

    fn indexed_workspace(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, content) in files {
            std::fs::write(dir.path().join(name), content).expect("write source");
        }
        let db = crate::graph::graph_db_path(dir.path());
        std::fs::create_dir_all(db.parent().expect("parent")).expect("mkdir");
        let graph = stella_graph::CodeGraph::open(dir.path(), &db).expect("open graph");
        graph.index_all().expect("index");
        graph.shutdown();
        dir
    }

    #[test]
    fn schema_is_read_only_and_named() {
        let schema = tool().schema();
        assert_eq!(schema.name, "read_symbol");
        assert!(schema.read_only);
    }

    /// The issue #330 witness: `read_symbol` returns exactly the symbol's
    /// `start..=end` body — the surrounding definitions never leak in.
    #[tokio::test]
    async fn returns_exactly_the_definitions_span() {
        let dir = indexed_workspace(&[("lib.rs", FIXTURE)]);
        let out = tool()
            .execute(&serde_json::json!({"name": "target_fn"}), dir.path())
            .await;
        match out {
            ToolOutput::Ok { content } => {
                assert!(
                    content.starts_with("fn target_fn (lib.rs:5-8)"),
                    "citation header with the exact span: {content}"
                );
                assert!(content.contains("5\tfn target_fn() {"), "{content}");
                assert!(content.contains("8\t}"), "{content}");
                assert!(!content.contains("alpha"), "no lines before: {content}");
                assert!(!content.contains("omega"), "no lines after: {content}");
                assert!(
                    content.contains("4/10 lines shown"),
                    "read through read_file's rendering: {content}"
                );
            }
            ToolOutput::Error { message } => panic!("expected the span, got: {message}"),
        }
    }

    #[tokio::test]
    async fn multiple_definitions_are_listed_never_silently_picked() {
        let dir = indexed_workspace(&[
            ("a.rs", "fn dup() {\n    let a = 1;\n}\n"),
            ("b.rs", "fn dup() {\n    let b = 2;\n}\n"),
        ]);
        let out = tool()
            .execute(&serde_json::json!({"name": "dup"}), dir.path())
            .await;
        match out {
            ToolOutput::Ok { content } => {
                assert!(content.contains("2 definitions"), "{content}");
                assert!(content.contains("- fn dup (a.rs:1-3)"), "{content}");
                assert!(content.contains("- fn dup (b.rs:1-3)"), "{content}");
                assert!(
                    content.contains("`path`"),
                    "tells the caller how to pick: {content}"
                );
                assert!(
                    !content.contains("let a") && !content.contains("let b"),
                    "no body was read: {content}"
                );
            }
            ToolOutput::Error { message } => {
                panic!("ambiguity is a listing, not an error: {message}")
            }
        }
    }

    #[tokio::test]
    async fn path_disambiguates_among_multiple_definitions() {
        let dir = indexed_workspace(&[
            ("a.rs", "fn dup() {\n    let a = 1;\n}\n"),
            ("b.rs", "fn dup() {\n    let b = 2;\n}\n"),
        ]);
        let out = tool()
            .execute(
                &serde_json::json!({"name": "dup", "path": "b.rs"}),
                dir.path(),
            )
            .await;
        match out {
            ToolOutput::Ok { content } => {
                assert!(content.starts_with("fn dup (b.rs:1-3)"), "{content}");
                assert!(content.contains("let b = 2"), "{content}");
                assert!(!content.contains("let a"), "{content}");
            }
            ToolOutput::Error { message } => panic!("path should disambiguate: {message}"),
        }

        // A path with no site for the name errors and lists where it IS.
        let miss = tool()
            .execute(
                &serde_json::json!({"name": "dup", "path": "c.rs"}),
                dir.path(),
            )
            .await;
        match miss {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("no definition in `c.rs`") || message.contains("c.rs"),
                    "{message}"
                );
                assert!(
                    message.contains("a.rs:1-3") && message.contains("b.rs:1-3"),
                    "{message}"
                );
            }
            ToolOutput::Ok { content } => panic!("a miss must not read anything: {content}"),
        }
    }

    #[tokio::test]
    async fn unknown_symbol_and_missing_name_are_named_errors() {
        let dir = indexed_workspace(&[("lib.rs", FIXTURE)]);
        let missing = tool()
            .execute(
                &serde_json::json!({"name": "no_such_symbol_xyz"}),
                dir.path(),
            )
            .await;
        match missing {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("no definition of `no_such_symbol_xyz`"),
                    "{message}"
                )
            }
            ToolOutput::Ok { content } => panic!("unknown symbol must error: {content}"),
        }
        let no_name = tool().execute(&serde_json::json!({}), dir.path()).await;
        assert!(no_name.is_error());
    }
}
