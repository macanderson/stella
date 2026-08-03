//! `graph_query` — query the workspace's code graph (tree-sitter symbols +
//! import edges, auto-indexed at session start into `.stella/private/codegraph.db`
//! and kept fresh by the live watcher).
//!
//! This is the runtime retrieval surface of `stella-graph`: instead of
//! grepping for a symbol, the agent asks the graph — where is `run_turn`
//! defined, who imports `src/auth.rs`, what is this file's neighborhood
//! (Field Manual Part 4: "code is a graph, not text").
//!
//! Registered UNCONDITIONALLY, unlike the issue/media tools: gating on the
//! index already existing would hide exactly the tool meant to create it
//! (`open_or_build` bootstraps one on first use) and leave the agent to
//! grep instead. The schema's token cost is paid once per session; a
//! chicken-and-egg gate cost every session on a fresh workspace.
//!
//! Read-only by construction: the tool opens the store, answers, and shuts
//! down per call (the same open/shutdown discipline as the schema gate),
//! so it never holds the SQLite file or a file watcher across turns.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::registry::Tool;

/// Frames rendered per query before eliding the tail — enough for every
/// realistic definition/importer list while bounding a pathological one
/// (e.g. `references` on a one-letter name) to a sane prompt size.
const MAX_FRAMES: usize = 30;

/// The index location `stella init` writes and the schema gate reads.
pub fn graph_db_path(root: &Path) -> PathBuf {
    root.join(".stella").join("private").join("codegraph.db")
}

/// Whether the workspace has an index — the condition for the code-map
/// footers and graph advisories (registration itself is unconditional). Resolver
/// failures stay distinct from absence so security errors cannot disable
/// graph-backed governance by masquerading as an uninitialized workspace.
pub fn graph_available(root: &Path) -> Result<bool, String> {
    stella_store::existing_workspace_private_sqlite_path(root, "codegraph.db")
        .map(|path| path.is_some())
        .map_err(|error| format!("cannot resolve private code graph state: {error}"))
}

/// Fallible storage-map assembly for every governance caller. The graph crate's
/// lower loader remains format-focused and best-effort; this boundary performs
/// private-state migration and rejects unsafe legacy layouts before delegating.
pub fn load_storage_snapshot(root: &Path) -> Result<stella_graph::StorageSnapshot, String> {
    stella_store::existing_workspace_private_sqlite_path(root, "codegraph.db")
        .map_err(|error| format!("cannot resolve private code graph state: {error}"))?;
    Ok(stella_graph::load_storage_snapshot(root))
}

pub struct CodeGraphQuery;

#[async_trait]
impl Tool for CodeGraphQuery {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "graph_query".into(),
            description: "Query the indexed code graph instead of grepping: where a symbol is \
                          defined or referenced, what a file imports, which files import it, or \
                          a file's full graph neighborhood. Cheaper and more precise than \
                          grep for symbol/dependency questions. The index builds automatically \
                          and refreshes live as files change — no manual re-index needed."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "op": {
                        "type": "string",
                        "enum": ["definitions", "references", "imports", "importers", "neighbors"],
                        "description": "definitions/references take a symbol name; \
                                        imports/importers/neighbors take a workspace-relative \
                                        file path"
                    },
                    "target": {
                        "type": "string",
                        "description": "The symbol name or file path to query"
                    }
                },
                "required": ["op", "target"]
            }),
            read_only: true,
            // `open_or_build` may bootstrap or catch up codegraph.db on the
            // read path — the in-tree example of a read that writes (#923).
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput {
        let op = input.get("op").and_then(|v| v.as_str()).unwrap_or("");
        let target = input.get("target").and_then(|v| v.as_str()).unwrap_or("");
        if target.is_empty() {
            return ToolOutput::Error {
                message: "`target` is required: a symbol name for definitions/references, a \
                          file path for imports/importers/neighbors"
                    .into(),
            };
        }
        // `run_query` opens SQLite and runs a full `index_all` catch-up pass,
        // which `stella_graph::CodeGraph::index_all`'s own contract says a
        // caller in an async context must wrap in `spawn_blocking` (#549).
        // The `CodeGraph` handle is created and dropped inside the closure —
        // only the rendered `ToolOutput` crosses back.
        let root = root.to_path_buf();
        let op = op.to_string();
        let target = target.to_string();
        tokio::task::spawn_blocking(move || run_query(&root, &op, &target))
            .await
            .unwrap_or_else(|_| ToolOutput::Error {
                message: "the code-graph query was cancelled".into(),
            })
    }
}

/// The single phrasing of the non-fatal index-pass diagnostic, shared by every
/// surface that reports it so the model and the operator read the same words
/// (and a test can assert on it without pinning the store's error text).
pub(crate) const INDEX_PASS_WARNING: &str = "warning: the code graph index pass \
     failed — answering from what the index already holds, which may be stale";

/// An open graph handle **plus** whatever non-fatal diagnostic its opening
/// catch-up pass produced.
///
/// A library must not own the process's stderr: Stella's primary surface is a
/// TUI, where a stray print to stderr paints raw text over the rendered frame
/// (issue #643). So the warning travels back to the caller as data instead,
/// and every caller has somewhere to put it — the tool output the model reads,
/// the `stella graph` output the operator reads, the overview JSON, or (for
/// impact selection) a stand-down note. It is never dropped on the floor.
pub(crate) struct OpenedGraph {
    pub(crate) graph: stella_graph::CodeGraph,
    /// `Some(message)` when the `index_all` catch-up pass failed and the
    /// answer therefore comes from whatever the index already held.
    pub(crate) index_warning: Option<String>,
}

/// Attach a non-fatal index warning to a rendered answer.
///
/// The warning goes **above** a successful answer — the model reads the caveat
/// before the possibly-stale frames it qualifies — and below a failure, where
/// the named error is the headline and the failed index pass is context for it.
pub(crate) fn with_index_warning(output: ToolOutput, warning: Option<String>) -> ToolOutput {
    let Some(warning) = warning else {
        return output;
    };
    match output {
        ToolOutput::Ok { content } => ToolOutput::Ok {
            content: format!("({warning})\n{content}"),
        },
        ToolOutput::Error { message } => ToolOutput::Error {
            message: format!("{message}\n({warning})"),
        },
    }
}

/// Open the code graph for a read, **building it on first use** when no
/// index exists yet.
///
/// The index is a session-start background build (`spawn_session_graph`), so
/// a query on turn 1 can race ahead of it. Bootstrapping here means the graph
/// tools are always advertised, and the first query that needs an index builds
/// one instead of erroring. `index_all` is the same pass `stella init` runs — a full build
/// on a fresh db, a hash-diff catch-up on an existing one — so it doubles as
/// the freshness pass that lets the graph see files the agent just wrote.
///
/// The `stale answers are worse than none` rule still holds: the pass runs
/// on every open, only a hard failure to prepare the store surfaces as an
/// error to the caller, and a failed pass is reported as
/// [`OpenedGraph::index_warning`] rather than silently tolerated.
pub(crate) fn open_or_build(root: &Path) -> Result<OpenedGraph, String> {
    // The WRITABLE path (creates `.stella/private/`), not the read-only
    // `existing_...` probe — this is the one place a query is allowed to
    // create the index it needs.
    let db_path = stella_store::workspace_private_sqlite_path(root, "codegraph.db")
        .map_err(|error| format!("cannot prepare the code graph store: {error}"))?;
    let graph = stella_graph::CodeGraph::open(root, &db_path)
        .map_err(|error| format!("could not open the code graph: {error}"))?;
    // A build/refresh failure is not fatal to a query: an existing index
    // still answers from its last good state, and a brand-new one answers
    // empty rather than aborting the agent's turn. It is still reported —
    // returned to the caller, never printed over the frame.
    let index_warning = graph
        .index_all()
        .err()
        .map(|error| format!("{INDEX_PASS_WARNING}: {error}"));
    Ok(OpenedGraph {
        graph,
        index_warning,
    })
}

/// Make the next `index_all` pass over `db` fail while leaving every read
/// answering — a trigger that aborts the pass's first row insert.
///
/// Tests only, and shared across this crate's modules (`graph`, `read_symbol`,
/// `gather` all thread the resulting warning). This is the shape of the real
/// failure the warning exists for: a store the pass cannot write to, over an
/// index that still holds usable rows. Every other way to break the pass —
/// an unwritable file or directory — breaks `CodeGraph::open` first and so
/// exercises the hard-error branch instead of this one.
///
/// The caller must also make the pass *attempt* a write (add or change an
/// indexable file); an unchanged tree is skipped by the byte-compat check
/// and never reaches the trigger.
#[cfg(test)]
pub(crate) fn block_index_writes(db: &Path) {
    let conn = rusqlite::Connection::open(db).expect("open the index for the test trigger");
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS stella_test_block_index_writes \
         BEFORE INSERT ON code_graph_files \
         BEGIN SELECT RAISE(ABORT, 'index writes blocked by the test'); END;",
    )
    .expect("install the test trigger");
}

/// Open → query → shutdown, entirely synchronous underneath (SQLite reads
/// plus the `open_or_build` catch-up pass). Shared by the tool and the
/// `stella graph` subcommand so both render the exact same frames.
///
/// **Synchronous — an async caller must wrap it in `spawn_blocking`**, the
/// same contract `stella_graph::CodeGraph::index_all` states.
///
/// A failed catch-up pass rides back out on the returned [`ToolOutput`], which
/// is what crosses the `spawn_blocking` boundary in `execute` — so the model
/// (tool call) and the operator (`stella graph`, which prints this content)
/// both see it.
pub fn run_query(root: &Path, op: &str, target: &str) -> ToolOutput {
    let OpenedGraph {
        graph,
        index_warning,
    } = match open_or_build(root) {
        Ok(opened) => opened,
        Err(message) => return ToolOutput::Error { message },
    };
    let output = run_query_with(&graph, op, target);
    graph.shutdown();
    with_index_warning(output, index_warning)
}

/// One query against an **already-open** handle, so a caller answering a
/// batch pays for a single open + `index_all` pass instead of one per query
/// (`gather_context` sweeps definitions AND references for every symbol).
/// Opening and shutting the handle is the caller's job.
pub(crate) fn run_query_with(
    graph: &stella_graph::CodeGraph,
    op: &str,
    target: &str,
) -> ToolOutput {
    let result = match op {
        "definitions" => graph.definitions(target),
        "references" => graph.references(target),
        "imports" => graph.imports_of(Path::new(target)),
        "importers" => graph.importers_of(Path::new(target)),
        "neighbors" => graph.neighbors(Path::new(target)),
        other => {
            return ToolOutput::Error {
                message: format!(
                    "unknown op `{other}` — expected definitions, references, imports, \
                     importers, or neighbors"
                ),
            };
        }
    };

    match result {
        // Importer edges only exist where import resolution succeeds:
        // relative TS/JS and Python paths, and Rust `use`/`mod` paths
        // through the module tree (#443). For everything else (Go, Java,
        // bare package specifiers) an empty importers answer is a
        // capability gap, not a stale index — saying "re-index" would send
        // the agent down a useless `stella init` retry.
        Ok(frames)
            if frames.is_empty()
                && op == "importers"
                && !(target.ends_with(".py") || target.ends_with(".rs")) =>
        {
            ToolOutput::Ok {
                content: format!(
                    "no importers found for `{target}` — importer edges exist only where \
                     import resolution succeeds (relative TS/JS/Python imports and Rust \
                     `use`/`mod` paths). Try `references` on the module name instead."
                ),
            }
        }
        Ok(frames) if frames.is_empty() => ToolOutput::Ok {
            content: format!(
                "no {op} found for `{target}` (index may be stale — `stella init` re-indexes)"
            ),
        },
        Ok(frames) => ToolOutput::Ok {
            content: render_frames(&frames),
        },
        Err(e) => ToolOutput::Error {
            message: format!("code-graph query failed: {e}"),
        },
    }
}

/// Render frames as cited entries — always the human citation label, never a
/// raw id (stella-graph L-C4) — eliding the tail loudly, never silently.
fn render_frames(frames: &[stella_graph::ContextFrame]) -> String {
    let mut lines: Vec<String> = frames
        .iter()
        .take(MAX_FRAMES)
        .map(|f| {
            let label = f.citation_label.as_deref().unwrap_or(&f.title);
            // CGP #33 made frame content optional (a `reference` frame carries
            // none); a contentless frame renders as its label alone.
            let content = f.content.as_deref().unwrap_or("").trim();
            if content.is_empty() {
                format!("- {label}")
            } else {
                format!("- {label}\n  {}", content.replace('\n', "\n  "))
            }
        })
        .collect();
    if frames.len() > MAX_FRAMES {
        lines.push(format!(
            "… (+{} more — narrow the query)",
            frames.len() - MAX_FRAMES
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indexed_workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn greet() -> &'static str { \"hi\" }\n",
        )
        .expect("write source");
        std::fs::write(dir.path().join("main.rs"), "mod lib;\nfn main() {}\n")
            .expect("write source");
        let db = graph_db_path(dir.path());
        std::fs::create_dir_all(db.parent().expect("parent")).expect("mkdir");
        let graph = stella_graph::CodeGraph::open(dir.path(), &db).expect("open graph");
        graph.index_all().expect("index");
        graph.shutdown();
        dir
    }

    #[cfg(unix)]
    fn legacy_indexed_workspace(dot_mode: u32) -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn legacy_greet() -> &'static str { \"hi\" }\n",
        )
        .expect("write source");
        let dot = dir.path().join(".stella");
        std::fs::create_dir_all(&dot).expect("mkdir");
        std::fs::set_permissions(&dot, std::fs::Permissions::from_mode(dot_mode)).unwrap();
        let legacy = dot.join("codegraph.db");
        let graph = stella_graph::CodeGraph::open(dir.path(), &legacy).expect("open graph");
        graph.index_all().expect("index");
        graph.shutdown();
        dir
    }

    #[test]
    fn schema_is_read_only_and_named() {
        let schema = CodeGraphQuery.schema();
        assert_eq!(schema.name, "graph_query");
        assert!(schema.read_only);
    }

    #[tokio::test]
    async fn a_query_with_no_index_builds_one_and_answers_rather_than_erroring() {
        // No `stella init`, no pre-existing db. A real source file is present,
        // so the on-first-use build indexes it and the query resolves the
        // symbol — the tool bootstraps the index it needs.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("greet.rs"), "pub fn greet() {}\n").unwrap();
        let out = CodeGraphQuery
            .execute(
                &serde_json::json!({"op": "definitions", "target": "greet"}),
                dir.path(),
            )
            .await;
        match out {
            ToolOutput::Ok { content } => {
                assert!(content.contains("greet"), "definition found: {content}");
            }
            ToolOutput::Error { message } => {
                panic!("first-use build should answer, not error: {message}")
            }
        }
        assert!(
            crate::graph::graph_db_path(dir.path()).exists(),
            "the index was built on first use"
        );
    }

    #[tokio::test]
    async fn definitions_finds_an_indexed_symbol_with_a_citation() {
        let dir = indexed_workspace();
        let out = CodeGraphQuery
            .execute(
                &serde_json::json!({"op": "definitions", "target": "greet"}),
                dir.path(),
            )
            .await;
        match out {
            ToolOutput::Ok { content } => {
                assert!(content.contains("greet"), "cites the symbol: {content}")
            }
            ToolOutput::Error { message } => panic!("expected frames, got: {message}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn direct_query_migrates_a_safe_legacy_index_before_opening_it() {
        let dir = legacy_indexed_workspace(0o700);
        let output = run_query(dir.path(), "definitions", "legacy_greet");
        match output {
            ToolOutput::Ok { content } => assert!(content.contains("legacy_greet")),
            ToolOutput::Error { message } => panic!("safe legacy index should migrate: {message}"),
        }
        assert!(!dir.path().join(".stella/codegraph.db").exists());
        assert!(graph_db_path(dir.path()).exists());
    }

    #[cfg(unix)]
    #[test]
    fn direct_query_reports_unsafe_legacy_index_instead_of_claiming_it_is_absent() {
        let dir = legacy_indexed_workspace(0o777);
        let output = run_query(dir.path(), "definitions", "legacy_greet");
        match output {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("legacy") && message.contains("private"),
                    "{message}"
                );
                assert!(!message.contains("no code graph index"), "{message}");
            }
            ToolOutput::Ok { .. } => panic!("unsafe legacy index must fail closed"),
        }
        assert!(dir.path().join(".stella/codegraph.db").exists());
    }

    #[cfg(unix)]
    #[test]
    fn availability_preflight_migrates_a_safe_legacy_index() {
        let dir = legacy_indexed_workspace(0o700);
        assert!(graph_available(dir.path()).unwrap());
        assert!(graph_db_path(dir.path()).exists());
    }

    #[tokio::test]
    async fn unknown_op_and_missing_target_are_named_errors() {
        let dir = indexed_workspace();
        let bad_op = CodeGraphQuery
            .execute(
                &serde_json::json!({"op": "callers", "target": "greet"}),
                dir.path(),
            )
            .await;
        assert!(matches!(bad_op, ToolOutput::Error { .. }));
        let no_target = CodeGraphQuery
            .execute(&serde_json::json!({"op": "definitions"}), dir.path())
            .await;
        assert!(matches!(no_target, ToolOutput::Error { .. }));
    }

    /// The #549 witness: `execute` must hand the synchronous open +
    /// `index_all` pass to the blocking pool rather than running it inline on
    /// a runtime worker.
    ///
    /// On the default single-threaded `#[tokio::test]` runtime a spawned task
    /// can only run while the test task is suspended at an await point. A
    /// body with no await at all — what `execute` was before this change —
    /// returns without ever yielding, so the flag is still `false`. Awaiting
    /// `spawn_blocking` yields, the spawned task runs, and the flag is set.
    #[tokio::test]
    async fn the_query_yields_the_runtime_while_the_index_pass_runs() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = indexed_workspace();
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        tokio::spawn(async move { flag.store(true, Ordering::SeqCst) });

        let out = CodeGraphQuery
            .execute(
                &serde_json::json!({"op": "definitions", "target": "greet"}),
                dir.path(),
            )
            .await;
        assert!(matches!(out, ToolOutput::Ok { .. }), "{out:?}");
        assert!(
            ran.load(Ordering::SeqCst),
            "graph_query blocked the runtime worker: a concurrently spawned task never got \
             to run while the code-graph index pass was in flight"
        );
    }

    /// The #643 witness, direct path: a failed catch-up pass must come back to
    /// the caller as text, not go to the process's stderr — where it would
    /// paint over the TUI frame. This is also the operator's path: `stella
    /// graph` prints exactly this content.
    #[test]
    fn a_failed_index_pass_returns_its_warning_to_the_caller() {
        let dir = indexed_workspace();
        block_index_writes(&graph_db_path(dir.path()));
        // Without a pending write the pass has nothing to abort on: the
        // byte-compat skip would let it succeed.
        std::fs::write(dir.path().join("added.rs"), "pub fn added_later() {}\n").expect("write");

        let output = run_query(dir.path(), "definitions", "greet");
        match output {
            ToolOutput::Ok { content } => {
                assert!(
                    content.contains(INDEX_PASS_WARNING),
                    "the index-pass failure must reach the caller, not stderr: {content}"
                );
                assert!(
                    content.contains("index writes blocked by the test"),
                    "the underlying store error is named: {content}"
                );
                // The whole point of the non-fatal branch: the last good index
                // still answers.
                assert!(
                    content.contains("greet"),
                    "the existing index still answers: {content}"
                );
            }
            ToolOutput::Error { message } => {
                panic!("a failed index pass is not fatal to a query: {message}")
            }
        }
    }

    /// The same warning must survive the `spawn_blocking` hop the tool takes,
    /// so the model — not just an in-process caller — reads it.
    #[tokio::test]
    async fn the_index_warning_crosses_the_spawn_blocking_boundary_to_the_model() {
        let dir = indexed_workspace();
        block_index_writes(&graph_db_path(dir.path()));
        std::fs::write(dir.path().join("added.rs"), "pub fn added_later() {}\n").expect("write");

        let out = CodeGraphQuery
            .execute(
                &serde_json::json!({"op": "definitions", "target": "greet"}),
                dir.path(),
            )
            .await;
        match out {
            ToolOutput::Ok { content } => assert!(
                content.contains(INDEX_PASS_WARNING) && content.contains("greet"),
                "{content}"
            ),
            ToolOutput::Error { message } => panic!("expected a warned answer, got: {message}"),
        }
    }

    /// A failing query that ALSO had a failing index pass must report both —
    /// the named error leads, the stale-index caveat follows it.
    #[test]
    fn a_failed_query_keeps_both_its_error_and_the_index_warning() {
        let warned = with_index_warning(
            ToolOutput::Error {
                message: "code-graph query failed: boom".into(),
            },
            Some(format!("{INDEX_PASS_WARNING}: disk on fire")),
        );
        match warned {
            ToolOutput::Error { message } => {
                assert!(
                    message.starts_with("code-graph query failed: boom"),
                    "{message}"
                );
                assert!(message.contains(INDEX_PASS_WARNING), "{message}");
            }
            ToolOutput::Ok { content } => panic!("an error must stay an error: {content}"),
        }
        // No warning, no noise: the common path is byte-identical.
        let clean = with_index_warning(
            ToolOutput::Ok {
                content: "frames".into(),
            },
            None,
        );
        assert!(matches!(clean, ToolOutput::Ok { content } if content == "frames"));
    }

    #[tokio::test]
    async fn empty_result_is_ok_with_a_stale_index_hint() {
        let dir = indexed_workspace();
        let out = CodeGraphQuery
            .execute(
                &serde_json::json!({"op": "definitions", "target": "no_such_symbol_xyz"}),
                dir.path(),
            )
            .await;
        match out {
            ToolOutput::Ok { content } => assert!(content.contains("no definitions")),
            ToolOutput::Error { message } => panic!("empty is not an error: {message}"),
        }
    }
}
