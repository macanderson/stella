//! `project_overview` — one call that answers "what is this repository?".
//!
//! Every other orientation tool in this crate is a batch executor for
//! questions the caller has already formed: `graph_query` needs a symbol or
//! file, `gather_context` needs patterns and globs, `grep` needs a regex.
//! None of them can be the *first* move, so an agent opening an unfamiliar
//! tree has no choice but to glob-and-read its way to a mental model — the
//! 10-30 call orientation loop this collapses into one.
//!
//! Assembly, not new capability: every field comes from a deterministic
//! source that already exists — the script index (static manifest
//! detection), the code graph, the storage/schema snapshot, and the domain
//! taxonomy. No model call, no shell, no grep.

use std::collections::BTreeSet;
use std::path::Path;

use async_trait::async_trait;
use serde_json::{Value, json};
use stella_protocol::{ToolOutput, ToolSchema};

use crate::registry::Tool;
use crate::scripts::ScriptIndex;

/// Deriving entry points costs one `importers_of` query per file, so it is
/// bounded: past this many files the roots list is omitted rather than
/// silently truncated into a half-truth.
const ENTRY_POINT_SCAN_LIMIT: usize = 400;

/// Entry points reported at most, newest-shallowest first.
const MAX_ENTRY_POINTS: usize = 12;

pub struct ProjectOverview;

#[async_trait]
impl Tool for ProjectOverview {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "project_overview".into(),
            description: "CALL THIS FIRST on an unfamiliar repository. Returns one JSON \
                          object describing the whole project — language and frameworks, \
                          the build/test/lint commands, entry-point files, the storage \
                          schema, domain taxonomy, and index freshness — assembled from \
                          static manifests and the code graph. Takes no arguments and \
                          costs no model call. Replaces the usual opening burst of \
                          glob/grep/read_file: use it before those, then reach for \
                          graph_query or gather_context once you know what to ask about."
                .into(),
            input_schema: json!({ "type": "object", "properties": {} }),
            // Read-only in the sense the flag means: it mutates no
            // workspace state, so speculative execution commutes with
            // everything around it. The index catch-up writes only to
            // Stella's own codegraph.db, which is invisible to the model and
            // serialized by the store's write guard.
            read_only: true,
        }
    }

    async fn execute(&self, _input: &Value, root: &Path) -> ToolOutput {
        ToolOutput::Ok {
            content: match serde_json::to_string_pretty(&build_overview(root)) {
                Ok(text) => text,
                Err(error) => {
                    return ToolOutput::Error {
                        message: format!("could not render the project overview: {error}"),
                    };
                }
            },
        }
    }
}

/// Assemble the overview. Total by construction: every source degrades to
/// its empty shape, because an orientation call that errors sends the agent
/// straight back to the glob loop this exists to replace.
pub fn build_overview(root: &Path) -> Value {
    let scripts = ScriptIndex::detect_blocking(root);
    let graph = open_graph(root);

    let mut out = json!({
        "workspace": root.display().to_string(),
        "scripts": scripts_section(&scripts),
        "domains": domains_section(root),
    });

    let map = out.as_object_mut().expect("object literal");
    match &graph {
        Some(graph) => {
            map.insert("index".into(), index_section(graph));
            map.insert("code".into(), code_section(graph));
            map.insert("storage".into(), storage_section(&graph.storage_snapshot()));
        }
        None => {
            // Say so plainly. A confident-looking object with silently empty
            // fields would read as "this project has no code".
            map.insert(
                "index".into(),
                json!({
                    "built": false,
                    "note": "no code graph index — run `stella init` to build one; \
                             language, entry points, and storage are unavailable until then",
                }),
            );
        }
    }
    out
}

fn open_graph(root: &Path) -> Option<stella_graph::CodeGraph> {
    // Build on first use, the same path `graph_query` takes: project_overview
    // is meant to be the FIRST call in a session, before the background index
    // build could possibly have finished, so it must be able to produce the
    // index it reports on rather than waiting for one to appear.
    crate::graph::open_or_build(root).ok()
}

fn index_section(graph: &stella_graph::CodeGraph) -> Value {
    json!({
        "built": true,
        "files": graph.file_count().unwrap_or(0),
        "symbols": graph.symbol_count().unwrap_or(0),
        "imports": graph.import_count().unwrap_or(0),
        // The index is a point-in-time build, so anything written since is
        // invisible here. Saying so is what keeps a stale answer from being
        // mistaken for a current one.
        "freshness": "caught up to the working tree when this call ran",
    })
}

fn code_section(graph: &stella_graph::CodeGraph) -> Value {
    let files = graph.all_files().unwrap_or_default();
    let mut languages: BTreeSet<String> = BTreeSet::new();
    for file in &files {
        if let Some(language) = language_of(file) {
            languages.insert(language.to_string());
        }
    }

    let mut section = json!({
        "languages": languages.into_iter().collect::<Vec<_>>(),
        "busiest_file": graph.busiest_file().unwrap_or(None),
    });
    let map = section.as_object_mut().expect("object literal");

    if files.len() > ENTRY_POINT_SCAN_LIMIT {
        map.insert(
            "entry_points".into(),
            json!(format!(
                "omitted — {} indexed files exceeds the {} scan limit; \
                 use graph_query importers to check a specific file",
                files.len(),
                ENTRY_POINT_SCAN_LIMIT
            )),
        );
        return section;
    }

    // A file nothing imports is a root: a binary, a script, a test, or dead
    // code — which is exactly the set worth reading first.
    let mut roots: Vec<String> = files
        .iter()
        .filter(|file| {
            graph
                .importers_of(Path::new(file))
                .map(|importers| importers.is_empty())
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    roots.sort_by_key(|path| (path.matches('/').count(), path.clone()));
    roots.truncate(MAX_ENTRY_POINTS);
    map.insert("entry_points".into(), json!(roots));
    section
}

fn storage_section(snapshot: &stella_graph::StorageSnapshot) -> Value {
    if snapshot.is_empty() {
        return json!({ "relations": 0 });
    }
    json!({
        "relations": snapshot.relations.len(),
        "layers": snapshot
            .layers
            .iter()
            .map(|layer| layer.key.clone())
            .collect::<Vec<_>>(),
        "relation_names": snapshot
            .relations
            .iter()
            .map(|relation| relation.address.clone())
            .collect::<Vec<_>>(),
    })
}

fn scripts_section(scripts: &ScriptIndex) -> Value {
    if scripts.is_empty() {
        return json!({ "detected": false });
    }
    let verbs: serde_json::Map<String, Value> = ["install", "build", "start", "test", "lint", "format"]
        .iter()
        .filter_map(|verb| {
            scripts
                .verb_entry(verb)
                .map(|entry| ((*verb).to_string(), json!(entry.command.clone())))
        })
        .collect();
    json!({
        "detected": true,
        "runners": scripts.detected_runners(),
        "primary_runner": scripts.primary_runner(),
        "verbs": verbs,
    })
}

/// The domain taxonomy `stella init` writes. Read straight off disk rather
/// than through `stella-cli`'s loader — this crate sits below it.
fn domains_section(root: &Path) -> Value {
    let path = root.join(".stella").join("domains.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return json!([]);
    };
    let Ok(parsed) = toml::from_str::<toml::Value>(&text) else {
        return json!([]);
    };
    let names: Vec<String> = parsed
        .get("domains")
        .and_then(|domains| domains.as_array())
        .map(|domains| {
            domains
                .iter()
                .filter_map(|domain| {
                    domain
                        .get("name")
                        .and_then(|name| name.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    json!(names)
}

/// Extension → language label, matching the grammars the indexer actually
/// carries. Anything else contributes no label rather than a guess.
fn language_of(path: &str) -> Option<&'static str> {
    let extension = Path::new(path).extension()?.to_str()?;
    Some(match extension {
        "rs" => "rust",
        "py" => "python",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "sql" => "sql",
        _ => return None,
    })
}


#[cfg(test)]
mod tests {
    use super::*;

    /// A truly empty workspace (no source at all) still answers, with an
    /// index that built but found nothing — never an error that would send
    /// the agent back to the glob loop this replaces.
    #[test]
    fn an_empty_workspace_answers_with_a_built_but_empty_index() {
        let dir = tempfile::tempdir().unwrap();
        let out = build_overview(dir.path());

        // The tool builds the index on first use, so it exists — and reports
        // zero files honestly rather than pretending there is nothing to index.
        assert_eq!(out["index"]["built"], serde_json::json!(true));
        assert_eq!(out["index"]["files"], serde_json::json!(0));
    }

    /// With real source present, the first call builds the index and the
    /// overview reports it — no prior `stella init`.
    #[test]
    fn a_first_call_builds_the_index_and_reports_real_symbols() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn f() {}\npub struct S;\n").unwrap();

        let out = build_overview(dir.path());
        assert_eq!(out["index"]["built"], serde_json::json!(true));
        assert!(
            out["index"]["files"].as_u64().unwrap_or(0) >= 1,
            "the first call indexed the source: {}",
            out["index"]
        );
        assert!(out.get("code").is_some(), "a code section is present: {out}");
    }

    #[test]
    fn build_scripts_are_reported_without_any_index() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let out = build_overview(dir.path());
        let scripts = &out["scripts"];
        assert_eq!(scripts["detected"], serde_json::json!(true));
        assert!(
            scripts["runners"]
                .as_array()
                .expect("runners")
                .iter()
                .any(|r| r == "cargo"),
            "cargo detected from the manifest alone: {scripts}"
        );
    }

    /// `domains.toml` is read straight off disk — this crate sits below the
    /// CLI that owns the loader, and the taxonomy is the agent's vocabulary
    /// for everything the graph tags.
    #[test]
    fn the_domain_taxonomy_is_read_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".stella")).unwrap();
        std::fs::write(
            dir.path().join(".stella").join("domains.toml"),
            "[[domains]]\nname = \"scheduling\"\n\n[[domains]]\nname = \"transport\"\n",
        )
        .unwrap();

        let out = build_overview(dir.path());
        assert_eq!(out["domains"], serde_json::json!(["scheduling", "transport"]));
    }

    #[test]
    fn a_malformed_taxonomy_degrades_to_empty_rather_than_failing_the_call() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".stella")).unwrap();
        std::fs::write(dir.path().join(".stella").join("domains.toml"), "not = [toml").unwrap();
        assert_eq!(build_overview(dir.path())["domains"], serde_json::json!([]));
    }
}
