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

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use async_trait::async_trait;
use serde_json::{Value, json};
use stella_protocol::{ToolOutput, ToolSchema};

use crate::registry::Tool;
use crate::scripts::ScriptIndex;

/// Entry points reported at most, shallowest first. The derivation is one
/// SQL anti-join (`CodeGraph::entry_points`) whatever the repository size,
/// so this bounds the rendered list, never the scan — a monorepo past a few
/// hundred files gets the same roots a small tree does.
const MAX_ENTRY_POINTS: usize = 12;

/// Top-level directories named in the layout summary at most. Past this the
/// remainder collapses to a count, so a monorepo with hundreds of packages
/// still renders a few dozen deterministic tokens.
const MAX_TOP_LEVEL_DIRS: usize = 12;

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
            // workspace state. NOT speculation-safe: the index catch-up
            // writes to Stella's own codegraph.db on the read path, which
            // is exactly the internal-state side effect a duplicate
            // speculative run must not repeat (#923).
            read_only: true,
            speculation_safe: false,
        }
    }

    async fn execute(&self, _input: &Value, root: &Path) -> ToolOutput {
        // `build_overview` is fully synchronous — manifest reads plus an
        // `open_or_build` that runs a whole `index_all` pass — so it goes to
        // the blocking pool, the form `ScriptIndex::detect` already uses in
        // this crate (#549). This tool is advertised as "CALL THIS FIRST", so
        // it is the one most likely to be occupying a worker on turn 1.
        let overview = {
            let root = root.to_path_buf();
            tokio::task::spawn_blocking(move || build_overview(&root)).await
        };
        let overview = match overview {
            Ok(overview) => overview,
            Err(_) => {
                return ToolOutput::Error {
                    message: "the project overview was cancelled".into(),
                };
            }
        };
        ToolOutput::Ok {
            content: match serde_json::to_string_pretty(&overview) {
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

/// A compact, deterministic orientation block for the system prompt, or
/// `None` only when the workspace is empty — the worker starts oriented, it
/// never has to *choose* to look.
///
/// Read-only on purpose: it opens an **existing** index and never builds one,
/// so it can be called during system-prompt assembly without ever blocking
/// the first response on an index build (which would defeat the point of a
/// fast first turn). When the index is absent or has indexed nothing — a
/// fresh session before the background build finishes, or a tree with no
/// files the indexer has a grammar for (an eight-trial bench run rendered
/// this block in zero worker prompts for exactly that reason) — it degrades
/// to `listing_orientation_block`, one bounded `read_dir` of the root,
/// instead of silently vanishing.
///
/// Deliberately the complement of the script index (which the prompt already
/// injects separately): languages, top-level layout, entry points, and
/// storage — the slow-churning skeleton the model would otherwise spend
/// grep/glob/read_file turns discovering. Fine detail that changes within a
/// session stays with `graph_query`/`read_file`; everything here is derived
/// from `codegraph.db`, so the block is byte-stable for a given index state
/// and kept to a few lines so it stays cheap in the cache-stable system
/// prefix. Every line is bounded by construction (issue #328): entry points
/// come from one SQL anti-join and the layout collapses past
/// `MAX_TOP_LEVEL_DIRS`, so a monorepo far beyond a few hundred files
/// renders the same useful map a small tree does.
pub fn render_orientation_block(root: &Path) -> Option<String> {
    graph_orientation_block(root).or_else(|| listing_orientation_block(root))
}

/// The graph-backed map — `None` when the index is absent or empty.
fn graph_orientation_block(root: &Path) -> Option<String> {
    let path = stella_store::existing_workspace_private_sqlite_path(root, "codegraph.db")
        .ok()
        .flatten()?;
    let graph = stella_graph::CodeGraph::open(root, &path).ok()?;

    let files = graph.file_count().unwrap_or(0);
    if files == 0 {
        return None;
    }

    let mut lines =
        vec!["## Project map (indexed — you do not need to grep/glob to find this)".to_string()];

    let all_files = graph.all_files().unwrap_or_default();
    let mut languages: BTreeSet<&'static str> = BTreeSet::new();
    for file in &all_files {
        if let Some(language) = language_of(file) {
            languages.insert(language);
        }
    }
    if !languages.is_empty() {
        lines.push(format!(
            "Languages: {}",
            languages.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }

    if let Some(layout) = top_level_summary(&all_files) {
        lines.push(layout);
    }

    let entry_points = graph.entry_points(MAX_ENTRY_POINTS).unwrap_or_default();
    if !entry_points.is_empty() {
        lines.push(format!("Entry points: {}", entry_points.join(", ")));
    }

    let storage = graph.storage_snapshot();
    if !storage.is_empty() {
        let layers: Vec<String> = storage.layers.iter().map(|l| l.key.clone()).collect();
        let layer_note = if layers.is_empty() {
            String::new()
        } else {
            format!(" across {}", layers.join(", "))
        };
        lines.push(format!(
            "Storage: {} relation(s){layer_note}",
            storage.relations.len()
        ));
    }

    // Only the header means nothing worth injecting.
    if lines.len() == 1 {
        return None;
    }
    Some(lines.join("\n"))
}

/// The graphless fallback: one sorted, bounded `read_dir` of the workspace
/// root. A tree the indexer has no grammar for (COBOL, nginx configs, a
/// tarball) still orients the worker from the first token — what it can see
/// at the top level — instead of leaving the prompt silently blank. Hidden
/// entries stay out (which also covers `.stella`), directories carry a
/// trailing `/`, and an empty workspace renders nothing: there is nothing to
/// orient toward, and the task text already says so.
fn listing_orientation_block(root: &Path) -> Option<String> {
    let mut entries: Vec<String> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            if name.starts_with('.') {
                return None;
            }
            let is_dir = entry.file_type().ok()?.is_dir();
            Some(if is_dir { format!("{name}/") } else { name })
        })
        .collect();
    if entries.is_empty() {
        return None;
    }
    entries.sort();
    let omitted = entries.len().saturating_sub(MAX_TOP_LEVEL_DIRS);
    entries.truncate(MAX_TOP_LEVEL_DIRS);
    let mut listing = format!("Top level: {}", entries.join(", "));
    if omitted > 0 {
        listing.push_str(&format!(", +{omitted} more"));
    }
    Some(format!(
        "## Project map (top-level listing — no code index yet)\n{listing}\n\
         No indexed symbols here yet; once code exists, project_overview \
         returns the graph-backed map."
    ))
}

/// Assemble the overview. Total by construction: every source degrades to
/// its empty shape, because an orientation call that errors sends the agent
/// straight back to the glob loop this exists to replace.
///
/// **Synchronous — an async caller must wrap it in `spawn_blocking`**: it
/// reads manifests and opens the code graph, which runs a full `index_all`
/// pass.
pub fn build_overview(root: &Path) -> Value {
    let scripts = ScriptIndex::detect_blocking(root);
    let graph = open_graph(root);

    let mut out = json!({
        "workspace": root.display().to_string(),
        "scripts": scripts_section(&scripts),
        "manifests": manifests_section(&scripts),
        "git": git_section(root),
        "domains": domains_section(root),
    });

    let map = out.as_object_mut().expect("object literal");
    match &graph {
        Some(opened) => {
            let graph = &opened.graph;
            map.insert(
                "index".into(),
                index_section(graph, opened.index_warning.as_deref()),
            );
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

/// Every package-manager manifest the script index parsed, with its path.
///
/// `scripts_section` reports the *verbs* those manifests bind (`build`,
/// `test`) and the runners behind them, which answers "how do I run this
/// project". It cannot answer "what kind of project is this and where are its
/// package boundaries" — a monorepo with a root `Cargo.toml`, three
/// `package.json` files and a `pyproject.toml` renders as two runner names.
///
/// The paths come from `ScriptEntry::source`, which the index already
/// resolved, so this is a projection rather than a second detection pass.
/// `synthesized` entries are dropped: they name an ecosystem default
/// (`cargo build --workspace`), not a file on disk, and a path that cannot be
/// opened is worse than an absent one.
fn manifests_section(scripts: &ScriptIndex) -> Value {
    let mut paths: BTreeSet<&str> = BTreeSet::new();
    for entry in &scripts.scripts {
        if entry.source != "synthesized" {
            paths.insert(entry.source.as_str());
        }
    }
    json!(paths.into_iter().collect::<Vec<_>>())
}

/// Remotes, branches, and the naming convention the existing branches imply.
///
/// Read with `git` rather than a library for the reason the rest of this
/// module assembles rather than computes: these are facts git already knows,
/// and a second implementation of ref parsing is a second thing to be wrong.
/// Absent or unreadable git state degrades to `{"repository": false}` — a
/// workspace that is not a repository is an ordinary state, not an error.
fn git_section(root: &Path) -> Value {
    let Some(remotes) = git_lines(root, &["remote", "-v"]) else {
        return json!({ "repository": false });
    };
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for line in &remotes {
        let mut parts = line.split_whitespace();
        if let (Some(name), Some(url)) = (parts.next(), parts.next()) {
            seen.entry(name.to_string())
                .or_insert_with(|| url.to_string());
        }
    }
    let branches = git_lines(
        root,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )
    .unwrap_or_default();

    json!({
        "repository": true,
        "remotes": seen.iter().map(|(n, u)| json!({"name": n, "url": u}))
            .collect::<Vec<_>>(),
        "branch_count": branches.len(),
        "branch_convention": branch_convention(&branches),
    })
}

/// The branch-name shape this repository already uses.
///
/// A new branch should look like the ones beside it, and the repository is the
/// only place that answer lives when no context record states a convention.
/// Reported as the dominant prefix segment (`feat/`, `fix/`, `worktree-`) plus
/// the separator, with the share of branches that follow it — a convention two
/// of nine branches use is a coincidence, and the count is what lets a reader
/// tell the difference rather than trusting a bare string.
///
/// `None` when nothing dominates. Guessing a convention from noise is worse
/// than admitting there is not one: a caller told "the convention is `x/`"
/// will follow it.
fn branch_convention(branches: &[String]) -> Value {
    let mut prefixes: BTreeMap<String, usize> = BTreeMap::new();
    for branch in branches {
        // First separator only: `feat/auth/login` is the `feat/` convention,
        // not a `feat/auth/` one.
        if let Some(idx) = branch.find(['/', '-']) {
            let sep = &branch[idx..idx + 1];
            let head = &branch[..idx];
            if !head.is_empty() {
                *prefixes.entry(format!("{head}{sep}")).or_default() += 1;
            }
        }
    }
    let total = branches.len();
    match prefixes.iter().max_by_key(|(_, n)| **n) {
        // Two is the floor for a pattern: one branch is an example, not a rule.
        Some((prefix, n)) if *n >= 2 => json!({
            "prefix": prefix,
            "branches_following": n,
            "branches_total": total,
        }),
        _ => Value::Null,
    }
}

/// Non-empty stdout lines from a `git` invocation, or `None` when git failed.
///
/// Synchronous on purpose: every caller here already runs on the blocking pool
/// (`build_overview` is dispatched there), and these are single ref reads.
fn git_lines(root: &Path, args: &[&str]) -> Option<Vec<String>> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect(),
    )
}

fn open_graph(root: &Path) -> Option<crate::graph::OpenedGraph> {
    // Build on first use, the same path `graph_query` takes: project_overview
    // is meant to be the FIRST call in a session, before the background index
    // build could possibly have finished, so it must be able to produce the
    // index it reports on rather than waiting for one to appear.
    crate::graph::open_or_build(root).ok()
}

fn index_section(graph: &stella_graph::CodeGraph, index_warning: Option<&str>) -> Value {
    let mut section = json!({
        "built": true,
        "files": graph.file_count().unwrap_or(0),
        "symbols": graph.symbol_count().unwrap_or(0),
        "imports": graph.import_count().unwrap_or(0),
        // The index is a point-in-time build, so anything written since is
        // invisible here. Saying so is what keeps a stale answer from being
        // mistaken for a current one.
        "freshness": "caught up to the working tree when this call ran",
    });
    // A failed catch-up pass makes the freshness claim above a lie, so it is
    // reported here — in the object the model reads — rather than printed over
    // the TUI frame (#643).
    if let Some(warning) = index_warning {
        let map = section.as_object_mut().expect("object literal");
        map.insert(
            "freshness".into(),
            json!("NOT caught up — the index pass for this call failed"),
        );
        map.insert("warning".into(), json!(warning));
    }
    section
}

fn code_section(graph: &stella_graph::CodeGraph) -> Value {
    let files = graph.all_files().unwrap_or_default();
    let mut languages: BTreeSet<String> = BTreeSet::new();
    for file in &files {
        if let Some(language) = language_of(file) {
            languages.insert(language.to_string());
        }
    }

    json!({
        "languages": languages.into_iter().collect::<Vec<_>>(),
        "busiest_file": graph.busiest_file().unwrap_or(None),
        "top_level": top_level_summary(&files),
        "entry_points": graph.entry_points(MAX_ENTRY_POINTS).unwrap_or_default(),
    })
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
    let verbs: serde_json::Map<String, Value> = crate::scripts::VERBS
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

/// One line summarizing where the code lives: top-level directories by
/// indexed-file count (largest first, ties by name), the remainder and any
/// root-level files collapsed to counts. Derived from the index's sorted
/// file list, so it is deterministic for a given index state — and it never
/// degrades: past `MAX_TOP_LEVEL_DIRS` the summary collapses instead of
/// disappearing, which is what keeps the injected map useful on a monorepo.
fn top_level_summary(files: &[String]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let mut dir_counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut root_files = 0usize;
    for file in files {
        match file.split_once('/') {
            Some((dir, _)) => *dir_counts.entry(dir).or_default() += 1,
            None => root_files += 1,
        }
    }
    let mut dirs: Vec<(&str, usize)> = dir_counts.into_iter().collect();
    dirs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let omitted = dirs.len().saturating_sub(MAX_TOP_LEVEL_DIRS);
    dirs.truncate(MAX_TOP_LEVEL_DIRS);

    let mut parts: Vec<String> = dirs
        .into_iter()
        .map(|(dir, count)| format!("{dir}/ ({count})"))
        .collect();
    if omitted > 0 {
        parts.push(format!("+{omitted} more dirs"));
    }
    if root_files > 0 {
        parts.push(format!("{root_files} at the root"));
    }
    Some(format!(
        "Layout ({} indexed files): {}",
        files.len(),
        parts.join(", ")
    ))
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

    /// The #643 witness for this tool: the overview's `freshness` line claims
    /// the index is caught up, so a failed catch-up pass has to retract that
    /// claim **in the JSON the model reads** rather than on the TUI's stderr.
    #[test]
    fn a_failed_index_pass_retracts_the_freshness_claim_in_the_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn f() {}\n").unwrap();
        // Build once so there is an index to answer from, then block the next
        // pass and give it a file it must write.
        assert_eq!(build_overview(dir.path())["index"]["built"], json!(true));
        crate::graph::block_index_writes(&crate::graph::graph_db_path(dir.path()));
        std::fs::write(dir.path().join("added.rs"), "pub fn added_later() {}\n").unwrap();

        let out = build_overview(dir.path());
        let index = &out["index"];
        assert_eq!(index["built"], json!(true), "{index}");
        assert!(
            index["warning"]
                .as_str()
                .is_some_and(|w| w.contains(crate::graph::INDEX_PASS_WARNING)),
            "the index-pass failure must reach the model: {index}"
        );
        assert!(
            index["freshness"]
                .as_str()
                .is_some_and(|f| f.contains("NOT caught up")),
            "a failed pass must not still claim freshness: {index}"
        );
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
        assert!(
            out.get("code").is_some(),
            "a code section is present: {out}"
        );
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
        // The fast-typecheck path is discoverable from the overview: the
        // `check` verb rides the same index the diagnostics tool uses.
        assert_eq!(
            scripts["verbs"]["check"],
            serde_json::json!("cargo check --workspace"),
            "{scripts}"
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
        assert_eq!(
            out["domains"],
            serde_json::json!(["scheduling", "transport"])
        );
    }

    /// A new branch should look like the ones beside it, and when no context
    /// record states a convention the repository is the only place that answer
    /// lives.
    #[test]
    fn the_dominant_branch_prefix_is_reported_with_its_share() {
        let branches: Vec<String> = ["feat/auth", "feat/billing", "feat/search", "main"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let out = branch_convention(&branches);
        assert_eq!(out["prefix"], "feat/");
        assert_eq!(out["branches_following"], 3);
        assert_eq!(out["branches_total"], 4);
    }

    /// A nested branch evidences its FIRST segment's convention.
    /// `feat/auth/login` is a `feat/` branch, never a `feat/auth/` rule.
    #[test]
    fn nesting_does_not_invent_a_deeper_convention() {
        let branches: Vec<String> = ["feat/auth/login", "feat/auth/logout"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert_eq!(branch_convention(&branches)["prefix"], "feat/");
    }

    /// Guessing from noise is worse than admitting there is no convention: a
    /// caller told "the convention is `x/`" will follow it. One branch is an
    /// example, not a rule.
    #[test]
    fn no_convention_is_reported_when_nothing_dominates() {
        assert!(branch_convention(&["main".to_string()]).is_null());
        assert!(branch_convention(&[]).is_null());
        assert!(
            branch_convention(&["feat/one".to_string(), "main".to_string()]).is_null(),
            "a single prefixed branch is an example, not a convention"
        );
    }

    /// A workspace that is not a git repository is an ordinary state, not an
    /// error — the overview still assembles around it.
    #[test]
    fn a_non_repository_says_so_rather_than_erroring() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(git_section(dir.path())["repository"], false);
    }

    #[test]
    fn orientation_block_without_an_index_lists_the_top_level_and_never_builds_one() {
        // Read-only: it must not create an index during system-prompt
        // assembly, or it would block the first response on a build. It still
        // orients — the pre-index degradation is a listing, not a blank.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn f() {}\n").unwrap();
        let block = render_orientation_block(dir.path()).expect("a non-empty tree orients");
        assert!(block.contains("no code index yet"), "{block}");
        assert!(block.contains("lib.rs"), "{block}");
        assert!(
            !crate::graph::graph_db_path(dir.path()).exists(),
            "the read-only block must not build an index"
        );
        // An empty workspace is the one case with nothing to say.
        let empty = tempfile::tempdir().unwrap();
        assert!(render_orientation_block(empty.path()).is_none());
    }

    /// The eight-trial bench shape: `stella init` built the index, but the
    /// workspace holds nothing the indexer has a grammar for (a tarball under
    /// deny-listed `vendor/`, COBOL sources). The empty graph must degrade to
    /// the top-level listing, never to a silently blank prompt — every worker
    /// prompt in that run went blank exactly here.
    #[test]
    fn an_empty_index_falls_back_to_the_top_level_listing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("vendor")).unwrap();
        std::fs::write(dir.path().join("vendor").join("src.tar.gz"), b"x").unwrap();
        std::fs::write(dir.path().join("main.cob"), "IDENTIFICATION DIVISION.\n").unwrap();
        let _ = build_overview(dir.path());
        let block = render_orientation_block(dir.path()).expect("the fallback renders");
        assert!(block.contains("no code index yet"), "{block}");
        assert!(block.contains("main.cob"), "{block}");
        assert!(block.contains("vendor/"), "{block}");
    }

    #[test]
    fn the_top_level_listing_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..MAX_TOP_LEVEL_DIRS + 3 {
            std::fs::write(dir.path().join(format!("f{i:02}")), b"").unwrap();
        }
        let block = render_orientation_block(dir.path()).expect("renders");
        assert!(block.contains("+3 more"), "{block}");
    }

    #[test]
    fn orientation_block_reports_languages_and_entry_points_from_an_existing_index() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.rs"),
            "mod helper;\npub fn main() {}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("helper.rs"), "pub fn help() {}\n").unwrap();
        // Build the index first (what the session background build / adapter
        // `stella init` does), THEN render read-only.
        let _ = build_overview(dir.path());

        let block = render_orientation_block(dir.path()).expect("an indexed workspace renders");
        assert!(block.contains("Project map"), "{block}");
        assert!(block.contains("Languages: rust"), "{block}");
        assert!(
            block.contains("Layout (2 indexed files): 2 at the root"),
            "the layout line summarizes where the code lives: {block}"
        );
        assert!(
            block.contains("Entry points:"),
            "a file nothing imports is an entry point: {block}"
        );
    }

    /// Issue #328 witness: past the old 400-file scan limit the map must stay
    /// useful — entry points are still derived (one SQL anti-join, no
    /// file-count cap) and the bounded layout line is still present, in both
    /// the tool output and the injected prompt block. Pre-#328, this fixture
    /// rendered no entry points anywhere and an "omitted" note in the tool.
    #[test]
    fn orientation_stays_useful_past_the_old_400_file_scan_limit() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(dir.path().join("main.rs"), "pub fn main() {}\n").unwrap();
        for i in 0..420 {
            std::fs::write(src.join(format!("f{i:03}.rs")), "pub fn f() {}\n").unwrap();
        }

        let out = build_overview(dir.path());
        assert!(
            out["index"]["files"].as_u64().unwrap_or(0) > 400,
            "the fixture must exceed the old scan limit: {}",
            out["index"]
        );
        let entry_points = out["code"]["entry_points"]
            .as_array()
            .expect("entry points stay a real list, never an 'omitted' note");
        assert_eq!(
            entry_points.first(),
            Some(&serde_json::json!("main.rs")),
            "shallowest first: {entry_points:?}"
        );

        let block = render_orientation_block(dir.path()).expect("an indexed workspace renders");
        assert!(block.contains("Entry points: main.rs"), "{block}");
        assert!(
            block.contains("Layout (421 indexed files): src/ (420), 1 at the root"),
            "{block}"
        );
    }

    #[test]
    fn a_malformed_taxonomy_degrades_to_empty_rather_than_failing_the_call() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".stella")).unwrap();
        std::fs::write(
            dir.path().join(".stella").join("domains.toml"),
            "not = [toml",
        )
        .unwrap();
        assert_eq!(build_overview(dir.path())["domains"], serde_json::json!([]));
    }

    /// The #549 witness for the "CALL THIS FIRST" tool: `execute` must hand
    /// the synchronous assembly (manifest reads + a full `index_all` pass) to
    /// the blocking pool instead of running it inline on a runtime worker.
    ///
    /// On the default single-threaded `#[tokio::test]` runtime a spawned task
    /// only runs while the test task is suspended at an await point. The old
    /// body had no await at all, so it returned without ever yielding and the
    /// flag stayed `false`; awaiting `spawn_blocking` lets the spawned task run.
    #[tokio::test]
    async fn the_overview_yields_the_runtime_while_the_index_pass_runs() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn f() {}\n").unwrap();
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        tokio::spawn(async move { flag.store(true, Ordering::SeqCst) });

        let out = ProjectOverview.execute(&json!({}), dir.path()).await;
        assert!(matches!(out, ToolOutput::Ok { .. }), "{out:?}");
        assert!(
            ran.load(Ordering::SeqCst),
            "project_overview blocked the runtime worker: a concurrently spawned task never \
             got to run while the overview was being assembled"
        );
    }
}
