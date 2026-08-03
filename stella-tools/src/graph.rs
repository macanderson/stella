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

/// Alternative spellings offered when a path target matches several indexed
/// files — enough to choose from without turning a miss into a file listing.
const MAX_PATH_CANDIDATES: usize = 5;

/// The language families the index recognizes, named in the not-indexed miss
/// so the agent learns the boundary once instead of re-probing it.
const INDEXED_LANGUAGES: &str =
    "rust, python, javascript, typescript, tsx, go, java, c, php, sql, prisma";

// ---------------------------------------------------------------------------
// Answer classification (#896)
//
// The acceptance criterion for making the code graph pull its weight is a
// *health metric*: what share of graph queries actually resolve. That number
// can only exist if a miss is machine-recognizable, and the only thing a
// telemetry reader has is the rendered answer text. So the exact phrases every
// miss branch emits live here as constants, each branch is built from one of
// them, and [`classify_answer`] is the single reader — prose and metric cannot
// drift apart, because there is only one copy of the prose.
// ---------------------------------------------------------------------------

/// Opens every "the graph holds nothing for this" answer.
pub const UNRESOLVED_MARKER: &str = "the code graph has no";
/// Marks a target that lies outside the one indexed root.
pub const OUT_OF_ROOT_MARKER: &str = "outside the indexed root";
/// Marks a target the indexer structurally does not cover.
pub const NOT_INDEXED_MARKER: &str = "is not a file type the code graph indexes";
/// Marks a path target that matches several indexed files.
pub const AMBIGUOUS_MARKER: &str = "matches several indexed files";

/// What a rendered `graph_query` answer actually was — the vocabulary of the
/// resolved-query-rate health metric (`stella stats graph`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphAnswer {
    /// Frames came back (possibly under a labelled fallback spelling).
    Resolved,
    /// The target is not under the indexed root — re-rooting is the fix, and
    /// no amount of re-indexing helps.
    OutOfRoot,
    /// The target exists but the indexer does not cover that file type.
    NotIndexed,
    /// The graph should plausibly have answered and did not — including an
    /// ambiguous path and the importer-resolution capability gap.
    Unresolved,
}

/// Classify one rendered `graph_query` answer.
///
/// Tolerant of the `(warning: …)` prefix a failed index pass adds above an
/// answer, because a stale-index warning qualifies an answer rather than
/// replacing it.
#[must_use]
pub fn classify_answer(content: &str) -> GraphAnswer {
    if content.contains(OUT_OF_ROOT_MARKER) {
        return GraphAnswer::OutOfRoot;
    }
    if content.contains(NOT_INDEXED_MARKER) {
        return GraphAnswer::NotIndexed;
    }
    if content.contains(AMBIGUOUS_MARKER) || content.contains(UNRESOLVED_MARKER) {
        return GraphAnswer::Unresolved;
    }
    GraphAnswer::Resolved
}

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
                          defined or referenced, what a definition calls (callees), which call \
                          sites name it (callers, best-effort by name), what a file imports, \
                          which files import it, or a file's full graph neighborhood. Cheaper \
                          and more precise than grep for symbol/dependency questions. The index \
                          builds automatically and refreshes live as files change — no manual \
                          re-index needed."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "op": {
                        "type": "string",
                        "enum": ["definitions", "references", "callees", "callers",
                                 "imports", "importers", "neighbors"],
                        "description": "definitions/references/callees/callers take a symbol \
                                        name; imports/importers/neighbors take a \
                                        workspace-relative file path"
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
                message: "`target` is required: a symbol name for \
                          definitions/references/callees/callers, a file path for \
                          imports/importers/neighbors"
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
    match op {
        "definitions" | "references" | "callees" | "callers" => symbol_query(graph, op, target),
        "imports" | "importers" | "neighbors" => path_query(graph, op, target),
        other => ToolOutput::Error {
            message: format!(
                "unknown op `{other}` — expected definitions, references, callees, callers, \
                 imports, importers, or neighbors"
            ),
        },
    }
}

/// The honesty label prefixed to every non-empty `callers` answer. The op is
/// a reverse NAME lookup over recorded call sites (#335 B1) — structurally a
/// call, but same-name methods and trait impls conflate, and calls through
/// function pointers or macro bodies are never seen — so it ships labeled
/// best-effort, the same discipline `references` applies through its score.
pub const CALLERS_BEST_EFFORT_NOTE: &str = "(callers matches call sites by name — same-name \
     methods and trait impls conflate; calls made through function pointers or macros are \
     not recorded)";

/// `definitions`/`references`/`callees`/`callers`: an exact-name lookup,
/// with one qualified-name retry behind it.
///
/// `store::definitions` matches `symbols.name` exactly, so every way a model
/// naturally writes a symbol it read in source — `Greeter::greet`,
/// `stella::graph::CodeGraph`, `graph.file_neighborhood` — resolves to zero
/// rows against an index that holds `greet`, `CodeGraph`,
/// `file_neighborhood`. Those are not stale-index misses; they are spelling
/// misses, and answering them with "`stella init` re-indexes" teaches the
/// model the tool is unreliable (#896).
///
/// Bounded on purpose: ONE retry, only on the last identifier segment, only
/// when the exact query already came back empty, and always labelled so the
/// model knows it did not get what it asked for (`x.id` → `id` can legitimately
/// return unrelated frames — [`MAX_FRAMES`] still caps them).
fn symbol_query(graph: &stella_graph::CodeGraph, op: &str, target: &str) -> ToolOutput {
    let lookup = |name: &str| match op {
        "definitions" => graph.definitions(name),
        "callees" => graph.callees(name),
        "callers" => graph.callers(name),
        _ => graph.references(name),
    };
    let frames = match lookup(target) {
        Ok(frames) => frames,
        Err(e) => return query_failed(e),
    };
    if !frames.is_empty() {
        return ToolOutput::Ok {
            content: render_symbol_frames(op, &frames),
        };
    }
    if let Some(segment) = last_identifier_segment(target)
        && let Ok(frames) = lookup(segment)
        && !frames.is_empty()
    {
        return ToolOutput::Ok {
            content: format!(
                "(`{target}` is not an indexed symbol name — showing `{segment}` instead)\n{}",
                render_symbol_frames(op, &frames)
            ),
        };
    }
    unresolved(op, target)
}

/// Render a symbol-op answer, prefixing the best-effort label on `callers`
/// (both the exact hit and the last-segment retry go through here, so the
/// label cannot be lost on one path).
fn render_symbol_frames(op: &str, frames: &[stella_graph::ContextFrame]) -> String {
    let rendered = render_frames(frames);
    if op == "callers" {
        format!("{CALLERS_BEST_EFFORT_NOTE}\n{rendered}")
    } else {
        rendered
    }
}

/// `imports`/`importers`/`neighbors`: a path lookup, with the two misses that
/// are NOT a stale index separated out first.
///
/// 1. A target outside the indexed root is named as such. The graph indexes
///    one root; a query against a sibling checkout or a `../` escape can never
///    resolve, and telling the agent to re-index is a dead end it will retry.
/// 2. A path spelled from a different vantage point — `packages/api/src/x.ts`
///    against a session rooted at `packages/api`, or a bare `graph.rs` — is
///    re-resolved against the indexed file list in either direction, which is
///    exactly the monorepo/sibling-tree spelling gap behind #896.
fn path_query(graph: &stella_graph::CodeGraph, op: &str, target: &str) -> ToolOutput {
    if let Some(content) = out_of_root_answer(graph.root(), target) {
        return ToolOutput::Ok { content };
    }
    let frames = match path_op(graph, op, target) {
        Ok(frames) => frames,
        Err(e) => return query_failed(e),
    };
    if !frames.is_empty() {
        return ToolOutput::Ok {
            content: render_frames(&frames),
        };
    }

    // The exact spelling found nothing. Is a differently-spelled indexed file
    // the one the agent meant?
    let mut indexed_spelling: Option<String> = None;
    match resolve_indexed_path(graph, target) {
        PathMatch::One(rel) => {
            if let Ok(frames) = path_op(graph, op, &rel)
                && !frames.is_empty()
            {
                return ToolOutput::Ok {
                    content: format!(
                        "(`{target}` is not an indexed path — showing `{rel}` instead)\n{}",
                        render_frames(&frames)
                    ),
                };
            }
            // The file IS indexed under `rel`; it simply has no edges of this
            // kind. That is a real answer about a known file, not a miss
            // against an unknown one — say which file it settled on.
            indexed_spelling = Some(rel);
        }
        PathMatch::Many(candidates) => {
            let shown: Vec<String> = candidates
                .iter()
                .take(MAX_PATH_CANDIDATES)
                .map(|c| format!("`{c}`"))
                .collect();
            let more = candidates.len().saturating_sub(shown.len());
            let tail = if more > 0 {
                format!(" (+{more} more)")
            } else {
                String::new()
            };
            return ToolOutput::Ok {
                content: format!(
                    "`{target}` {AMBIGUOUS_MARKER}: {}{tail} — re-query with one of them.",
                    shown.join(", ")
                ),
            };
        }
        PathMatch::None => {}
    }

    // Importer edges only exist where import resolution succeeds: relative
    // TS/JS and Python paths, and Rust `use`/`mod` paths through the module
    // tree (#443). For everything else (Go, Java, bare package specifiers) an
    // empty importers answer is a capability gap, not a stale index — saying
    // "re-index" would send the agent down a useless `stella init` retry.
    if op == "importers" && !(target.ends_with(".py") || target.ends_with(".rs")) {
        return ToolOutput::Ok {
            content: format!(
                "{UNRESOLVED_MARKER} importers for `{target}` — importer edges exist only where \
                 import resolution succeeds (relative TS/JS/Python imports and Rust \
                 `use`/`mod` paths). Try `references` on the module name instead."
            ),
        };
    }
    if let Some(content) = unindexed_language_answer(graph.root(), target) {
        return ToolOutput::Ok { content };
    }
    match indexed_spelling {
        Some(rel) => ToolOutput::Ok {
            content: format!("{UNRESOLVED_MARKER} {op} for `{target}` (indexed as `{rel}`)"),
        },
        None => unresolved(op, target),
    }
}

/// Dispatch one already-validated path op.
fn path_op(
    graph: &stella_graph::CodeGraph,
    op: &str,
    target: &str,
) -> Result<Vec<stella_graph::ContextFrame>, stella_graph::GraphError> {
    let path = Path::new(target);
    match op {
        "imports" => graph.imports_of(path),
        "importers" => graph.importers_of(path),
        _ => graph.neighbors(path),
    }
}

fn query_failed(e: stella_graph::GraphError) -> ToolOutput {
    ToolOutput::Error {
        message: format!("code-graph query failed: {e}"),
    }
}

/// The one generic miss — the only branch entitled to blame the index.
fn unresolved(op: &str, target: &str) -> ToolOutput {
    ToolOutput::Ok {
        content: format!(
            "{UNRESOLVED_MARKER} {op} for `{target}` (index may be stale — \
             `stella init` re-indexes)"
        ),
    }
}

/// Name a target that lies outside the indexed root instead of blaming the
/// index for not holding it. Mirrors the wording `bash`'s cd-drift note
/// already uses, so the two surfaces teach the same fix.
///
/// `None` means "not out of root, carry on" — including the case where `root`
/// itself cannot be canonicalized, which [`crate::resolve_within_root`] also
/// reports as `None`. Conflating the two would make a legitimate query claim
/// it escaped.
fn out_of_root_answer(root: &Path, target: &str) -> Option<String> {
    if root.canonicalize().is_err() {
        return None;
    }
    if crate::resolve_within_root(root, target).is_some() {
        return None;
    }
    Some(format!(
        "`{target}` is {OUT_OF_ROOT_MARKER} `{}` — the code graph indexes only this root, so \
         nothing under that path is in it and re-indexing will not add it. To query that tree, \
         re-root the session on it (run stella from it).",
        root.display()
    ))
}

/// Distinguish "the code graph does not index this kind of file" from "indexed,
/// no edges". A `.md`/`.toml`/`.yaml` target is a permanent non-answer; sending
/// the agent to `stella init` for it is the miss that teaches the tool is
/// unreliable.
fn unindexed_language_answer(root: &Path, target: &str) -> Option<String> {
    let full = crate::resolve_within_root(root, target)?;
    if !full.is_file() {
        return None;
    }
    if stella_graph::Language::from_path(&full).is_some()
        || stella_graph::storage::indexes_without_language(&full)
    {
        return None;
    }
    Some(format!(
        "`{target}` {NOT_INDEXED_MARKER} ({INDEXED_LANGUAGES}) — read it with read_file or \
         search it with grep; re-indexing will not add it."
    ))
}

/// The last `::`/`.`-separated identifier segment of a qualified symbol name,
/// or `None` when `target` is not identifier-shaped or carries no separator.
/// Segment rules come from [`crate::code_map::is_identifier_or_path`] rather
/// than a second splitter, so the retry and the grep nudge agree on what a
/// symbol name looks like.
fn last_identifier_segment(target: &str) -> Option<&str> {
    if !crate::code_map::is_identifier_or_path(target) {
        return None;
    }
    let segment = target.rsplit(['.', ':']).find(|s| !s.is_empty())?;
    (segment != target).then_some(segment)
}

/// How a caller-supplied path relates to the indexed file list.
enum PathMatch {
    /// Exactly one indexed file is a plausible re-spelling.
    One(String),
    /// Several are — the agent must disambiguate.
    Many(Vec<String>),
    None,
}

/// Re-resolve a path the index did not recognize against the paths it holds,
/// in **both** directions:
///
/// * the indexed path ends with the target — a bare filename or a tail
///   (`graph.rs`, `src/x.ts`);
/// * the target ends with the indexed path — a path written from an ancestor's
///   vantage point (`packages/api/src/x.ts` in a session rooted at
///   `packages/api`). This is the monorepo/sibling-tree spelling that silently
///   returned nothing before #896.
///
/// Only reached on a miss, so the full file list is read once per failed query
/// rather than per query.
fn resolve_indexed_path(graph: &stella_graph::CodeGraph, target: &str) -> PathMatch {
    let needle = normalize_slash(target);
    if needle.is_empty() {
        return PathMatch::None;
    }
    let Ok(files) = graph.all_files() else {
        return PathMatch::None;
    };
    let mut hits: Vec<String> = files
        .into_iter()
        .filter(|f| {
            // An exact match already ran and came back empty; re-running it
            // under a "did you mean" label would be a lie.
            *f != needle
                && (f.ends_with(&format!("/{needle}")) || needle.ends_with(&format!("/{f}")))
        })
        .collect();
    match hits.len() {
        0 => PathMatch::None,
        1 => PathMatch::One(hits.remove(0)),
        _ => {
            hits.sort();
            PathMatch::Many(hits)
        }
    }
}

/// Root-relative forward-slash spelling of a caller path: `\` → `/`, and no
/// leading `./` or `/` or trailing `/`. The index stores paths this way.
fn normalize_slash(target: &str) -> String {
    target
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string()
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
        // `rename` is the op the graph deliberately does NOT offer (#335:
        // name-based edges cannot make a rename safe).
        let bad_op = CodeGraphQuery
            .execute(
                &serde_json::json!({"op": "rename", "target": "greet"}),
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

    // ------------------------------------------------------------------
    // #896: an unresolved query must name its real cause. Blaming a stale
    // index for a miss that re-indexing cannot fix is what teaches the model
    // the tool is unreliable, and an unreliable tool is an unused tool.
    // ------------------------------------------------------------------

    fn ok_content(output: ToolOutput) -> String {
        match output {
            ToolOutput::Ok { content } => content,
            ToolOutput::Error { message } => panic!("expected an answer, got: {message}"),
        }
    }

    /// A workspace laid out like a monorepo package, plus a real sibling tree
    /// beside it that the session root does NOT cover.
    fn monorepo_workspace() -> tempfile::TempDir {
        let outer = tempfile::tempdir().expect("tempdir");
        let root = outer.path().join("api");
        std::fs::create_dir_all(root.join("packages/core/src")).expect("mkdir");
        std::fs::create_dir_all(root.join("packages/web/src")).expect("mkdir");
        std::fs::write(
            root.join("packages/core/src/util.rs"),
            "pub fn core_util() {}\n",
        )
        .expect("write source");
        std::fs::write(
            root.join("packages/web/src/util.rs"),
            "pub fn web_util() {}\n",
        )
        .expect("write source");
        std::fs::write(root.join("packages/core/README.md"), "# core\n").expect("write doc");
        // The sibling checkout: real source, real files, and permanently
        // outside the indexed root.
        std::fs::create_dir_all(outer.path().join("sibling/src")).expect("mkdir");
        std::fs::write(
            outer.path().join("sibling/src/lib.rs"),
            "pub fn sibling_fn() {}\n",
        )
        .expect("write source");

        let db = graph_db_path(&root);
        std::fs::create_dir_all(db.parent().expect("parent")).expect("mkdir");
        let graph = stella_graph::CodeGraph::open(&root, &db).expect("open graph");
        graph.index_all().expect("index");
        graph.shutdown();
        outer
    }

    /// The primary #896 witness. Before this change every miss — including one
    /// pointing at a sibling checkout the graph structurally cannot hold —
    /// came back as "index may be stale — `stella init` re-indexes", which is
    /// a dead-end retry loop.
    #[test]
    fn a_target_outside_the_indexed_root_is_named_not_blamed_on_a_stale_index() {
        let outer = monorepo_workspace();
        let root = outer.path().join("api");
        let content = ok_content(run_query(&root, "neighbors", "../sibling/src/lib.rs"));
        assert!(
            content.contains(OUT_OF_ROOT_MARKER),
            "the miss must name the root boundary: {content}"
        );
        // The answer names the graph's own canonical root (macOS temp dirs
        // are symlinks, so the raw fixture path is not what it prints).
        let canonical = root.canonicalize().expect("canonical root");
        assert!(
            content.contains(&canonical.display().to_string()),
            "the indexed root is named so the fix is actionable: {content}"
        );
        assert!(
            !content.contains("index may be stale"),
            "re-indexing cannot pull in a tree outside the root: {content}"
        );
        assert_eq!(classify_answer(&content), GraphAnswer::OutOfRoot);
    }

    /// #335 B1: the call-edge ops. `callees` answers from the call sites
    /// recorded inside the definition's span; `callers` reverse-looks-up the
    /// name and carries its best-effort label on every answer.
    #[test]
    fn callees_and_callers_answer_from_the_call_table() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn greet() -> &'static str { \"hi\" }\n\
             pub fn salute() {\n    greet();\n    format_name();\n}\n",
        )
        .expect("write source");

        let callees = ok_content(run_query(dir.path(), "callees", "salute"));
        assert!(callees.contains("greet"), "{callees}");
        assert!(callees.contains("format_name"), "{callees}");
        assert!(
            callees.contains("callees of fn salute"),
            "the frame cites the definition: {callees}"
        );
        assert_eq!(classify_answer(&callees), GraphAnswer::Resolved);

        let callers = ok_content(run_query(dir.path(), "callers", "greet"));
        assert!(
            callers.contains(CALLERS_BEST_EFFORT_NOTE),
            "every callers answer carries its honesty label: {callers}"
        );
        assert!(
            callers.contains("fn salute (lib.rs:3)"),
            "the call site is labeled by its enclosing definition: {callers}"
        );
        assert_eq!(classify_answer(&callers), GraphAnswer::Resolved);

        // A name nothing calls is an ordinary unresolved miss, countable by
        // the health metric like every other miss.
        let miss = ok_content(run_query(dir.path(), "callers", "greet_nobody"));
        assert_eq!(classify_answer(&miss), GraphAnswer::Unresolved);
    }

    /// The exact-match SQL behind `definitions` never sees a qualified name,
    /// so every `Type::method` a model copies out of source used to resolve to
    /// zero rows.
    #[test]
    fn a_method_qualified_symbol_falls_back_to_its_last_segment() {
        let dir = indexed_workspace();
        let content = ok_content(run_query(dir.path(), "definitions", "Greeter::greet"));
        assert!(
            content.contains("greet"),
            "the last segment resolves: {content}"
        );
        assert!(
            content.contains("not an indexed symbol name"),
            "the substitution is labelled, never silent: {content}"
        );
        assert_eq!(classify_answer(&content), GraphAnswer::Resolved);
    }

    /// A path written from the enclosing repo's vantage point — the monorepo
    /// spelling — must resolve against the file the index actually holds.
    #[test]
    fn a_path_spelled_from_an_ancestor_vantage_resolves_to_the_indexed_file() {
        let outer = monorepo_workspace();
        let root = outer.path().join("api");
        let content = ok_content(run_query(
            &root,
            "neighbors",
            "services/api/packages/core/src/util.rs",
        ));
        assert!(
            content.contains("core_util"),
            "the indexed file answers: {content}"
        );
        assert!(
            content.contains("not an indexed path"),
            "the substitution is labelled: {content}"
        );
        assert_eq!(classify_answer(&content), GraphAnswer::Resolved);
    }

    /// A bare filename that several indexed files could be is a disambiguation
    /// prompt, not a stale-index claim.
    #[test]
    fn an_ambiguous_bare_filename_lists_the_indexed_candidates() {
        let outer = monorepo_workspace();
        let root = outer.path().join("api");
        let content = ok_content(run_query(&root, "neighbors", "util.rs"));
        assert!(content.contains(AMBIGUOUS_MARKER), "{content}");
        assert!(content.contains("packages/core/src/util.rs"), "{content}");
        assert!(content.contains("packages/web/src/util.rs"), "{content}");
        assert!(!content.contains("index may be stale"), "{content}");
        assert_eq!(classify_answer(&content), GraphAnswer::Unresolved);
    }

    /// A file type the indexer does not cover is a permanent non-answer.
    #[test]
    fn an_unindexed_file_type_is_named_rather_than_sent_to_stella_init() {
        let outer = monorepo_workspace();
        let root = outer.path().join("api");
        let content = ok_content(run_query(&root, "neighbors", "packages/core/README.md"));
        assert!(content.contains(NOT_INDEXED_MARKER), "{content}");
        assert!(!content.contains("index may be stale"), "{content}");
        assert_eq!(classify_answer(&content), GraphAnswer::NotIndexed);
    }

    /// The metric and the prose share one set of literals — this walks every
    /// branch that emits one through the classifier, so a reworded miss that
    /// stopped being countable would fail here rather than silently deflate
    /// the resolved-query rate.
    #[test]
    fn every_answer_branch_classifies_the_way_the_report_reads_it() {
        let outer = monorepo_workspace();
        let root = outer.path().join("api");

        let cases: [(&str, &str, GraphAnswer); 6] = [
            // Resolved: real frames.
            ("definitions", "core_util", GraphAnswer::Resolved),
            // Out of root: a sibling checkout.
            ("imports", "../sibling/src/lib.rs", GraphAnswer::OutOfRoot),
            // Not indexed: a markdown file that really is in the workspace.
            (
                "neighbors",
                "packages/core/README.md",
                GraphAnswer::NotIndexed,
            ),
            // Unresolved, ambiguous spelling.
            ("neighbors", "util.rs", GraphAnswer::Unresolved),
            // Unresolved, generic miss.
            ("definitions", "nothing_named_this", GraphAnswer::Unresolved),
            // Unresolved, importer-resolution capability gap.
            (
                "importers",
                "packages/core/src/util.go",
                GraphAnswer::Unresolved,
            ),
        ];
        for (op, target, expected) in cases {
            let content = ok_content(run_query(&root, op, target));
            assert_eq!(
                classify_answer(&content),
                expected,
                "{op} `{target}` classified wrong: {content}"
            );
        }
    }

    /// The warning prefix qualifies an answer; it must not reclassify it.
    #[test]
    fn an_index_warning_prefix_does_not_change_the_classification() {
        let warned = format!("({INDEX_PASS_WARNING}: boom)\n- lib.rs::greet\n  pub fn greet()");
        assert_eq!(classify_answer(&warned), GraphAnswer::Resolved);
        let warned_miss =
            format!("({INDEX_PASS_WARNING}: boom)\n{UNRESOLVED_MARKER} definitions for `x`");
        assert_eq!(classify_answer(&warned_miss), GraphAnswer::Unresolved);
    }

    #[test]
    fn only_a_qualified_identifier_earns_a_last_segment_retry() {
        assert_eq!(last_identifier_segment("Greeter::greet"), Some("greet"));
        assert_eq!(
            last_identifier_segment("graph.file_neighborhood"),
            Some("file_neighborhood")
        );
        assert_eq!(last_identifier_segment("greet"), None, "no separator");
        assert_eq!(
            last_identifier_segment("src/lib.rs"),
            None,
            "a path is not a symbol name"
        );
        assert_eq!(last_identifier_segment("a-b.c"), None, "not identifiers");
    }
}
