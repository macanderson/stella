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
//! detection), the code graph, the storage/schema snapshot, the domain
//! taxonomy, and the version-control state (`repository_section`). No
//! model call, no grep, and no shell: the git reads are direct argv spawns
//! of one pinned binary, never a command string.

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
                          schema, domain taxonomy, index freshness, and the version-control \
                          state (current branch or DETACHED position, uncommitted and \
                          shelved/stashed changes, branches with ahead/behind vs the \
                          default, and the last 10 commits) — assembled from static \
                          manifests, the code graph and git. Takes no arguments and costs \
                          no model call. Replaces the usual opening burst of \
                          glob/grep/read_file: use it before those, then reach for \
                          graph_query or gather_context once you know what to ask about, \
                          or repo_history / repo_recover to go deeper on git."
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
                return ToolOutput::Error { class: None,
                    message: "the project overview was cancelled".into(),
                };
            }
        };
        ToolOutput::Ok {
            content: match serde_json::to_string_pretty(&overview) {
                Ok(text) => text,
                Err(error) => {
                    return ToolOutput::Error { class: None,
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

/// Recent commits named in the `repository` section. Ten is the ask a
/// person actually makes ("what were the last ten commits?"); `repo_history`
/// goes deeper on request.
const OVERVIEW_COMMITS: usize = 10;

/// Branches placed against the default branch here at most. Each one past
/// the first costs a `rev-list --count` spawn, so this bounds *work* — the
/// full list is `repo_history`'s job.
const OVERVIEW_BRANCHES: usize = 8;

/// Non-empty stdout lines from one `git` read, or `None` when git is absent,
/// the directory is not a repository, or the command failed.
///
/// Direct argv — no shell, nothing interpolated into a command string.
///
/// Synchronous on purpose: every caller here already runs on the blocking
/// pool (`build_overview` is dispatched there), and these are single ref
/// reads.
///
/// `current_dir` is **not** sufficient to say which repository this reads.
/// `GIT_DIR` overrides it, and git *exports* `GIT_DIR` to every hook it runs —
/// so an overview assembled from inside a pre-push hook would report the outer
/// repository's branches, remotes and languages as this workspace's, silently
/// and with no error to notice. `an_ambient_git_dir_cannot_retarget_the_overview`
/// is the witness.
///
/// [`scrub_spawn_std_env`](crate::subprocess_env::scrub_spawn_std_env) is the
/// whole answer and the only correct call here: it removes the retargeting
/// family (`GIT_REPO_ENV_VARS`) and the forced-colour family *before*
/// delegating to the credential/ambient-authority scrub. Reaching for
/// `scrub_sensitive_std_env` and re-removing `GIT_REPO_ENV_VARS` by hand is
/// the exact partial application that helper exists to prevent — and it
/// silently drops the colour half, which matters because this output goes to
/// a captured pipe rather than a terminal.
fn git_lines(root: &Path, args: &[&str]) -> Option<Vec<String>> {
    let mut command = std::process::Command::new("git");
    command
        .args(args)
        .current_dir(root)
        // Nothing here reads stdin; inheriting it lets a git that decides to
        // prompt (a credential helper on a remote URL) hang the overview.
        .stdin(std::process::Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0");
    crate::subprocess_env::scrub_spawn_std_env(&mut command);
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// Version-control orientation: where the working position is, what is
/// uncommitted, what is shelved, which branches exist and how they sit
/// against the default, and the last [`OVERVIEW_COMMITS`] commits.
///
/// This is the fifth deterministic source (beside the script index, the code
/// graph, the storage snapshot and the domain taxonomy) and the module doc's
/// "no shell" still holds literally: these are direct argv spawns of one
/// pinned binary, never a shell string.
///
/// It earns its place because the questions it answers were previously
/// reachable only through `bash` — and a read-only role does not have
/// `bash`. A `fix-git` bench trial opened with `project_overview`, learned
/// nothing about git from it, and spent the next 45 seconds failing to find
/// a tool that could tell it. `detached` alone would have ended that search.
///
/// It also carries the remotes and the branch-naming convention the existing
/// branches imply — one section, because two sibling keys each answering
/// "what does git say about this tree" is the same false-orientation defect
/// in a politer form. #2551 and this change arrived at the same payload from
/// opposite ends and merged without a textual conflict; the union is stated
/// once, here.
///
/// `{"repository": false}` when this is not a repository — stated plainly,
/// because an absent key reads as "the overview forgot" rather than "there
/// is no version control here".
fn git_section(root: &Path) -> Value {
    // `remote -v` doubles as the is-this-a-repository probe: it is the
    // cheapest command that fails outside a work tree and is needed anyway.
    let Some(remote_lines) = git_lines(root, &["remote", "-v"]) else {
        return json!({ "repository": false });
    };
    let mut remotes: BTreeMap<String, String> = BTreeMap::new();
    for line in &remote_lines {
        let mut parts = line.split_whitespace();
        if let (Some(name), Some(url)) = (parts.next(), parts.next()) {
            remotes
                .entry(name.to_string())
                .or_insert_with(|| url.to_string());
        }
    }

    let branch = git_lines(root, &["symbolic-ref", "-q", "--short", "HEAD"])
        .and_then(|lines| lines.into_iter().next())
        .filter(|b| !b.is_empty());
    let default_branch = git_lines(root, &["rev-parse", "--abbrev-ref", "origin/HEAD"])
        .and_then(|lines| lines.into_iter().next())
        .and_then(|full| full.split_once('/').map(|(_, name)| name.to_string()))
        .filter(|b| !b.is_empty());

    let count = OVERVIEW_COMMITS.to_string();
    let commits: Vec<Value> = git_lines(
        root,
        &["log", "--no-color", "--max-count", &count, "--format=%h %s"],
    )
    .unwrap_or_default()
    .into_iter()
    .map(Value::from)
    .collect();

    let shelved: Vec<Value> = git_lines(root, &["stash", "list", "--format=%gd %s"])
        .unwrap_or_default()
        .into_iter()
        .map(Value::from)
        .collect();

    let names = git_lines(
        root,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )
    .unwrap_or_default();
    let branch_total = names.len();
    let convention = branch_convention(&names);
    let mut branches = Vec::new();
    for name in names.iter().take(OVERVIEW_BRANCHES) {
        let mut row = json!({ "name": name, "current": Some(name) == branch.as_ref() });
        if let Some(base) = default_branch.as_deref()
            && base != name.as_str()
            && let Some(counts) = git_lines(
                root,
                &[
                    "rev-list",
                    "--left-right",
                    "--count",
                    &format!("{base}...{name}"),
                ],
            )
            .and_then(|lines| lines.into_iter().next())
        {
            let mut cols = counts.split_whitespace();
            let behind = cols.next().and_then(|c| c.parse::<u64>().ok());
            let ahead = cols.next().and_then(|c| c.parse::<u64>().ok());
            if let (Some(ahead), Some(behind)) = (ahead, behind) {
                let map = row.as_object_mut().expect("object literal");
                map.insert("ahead".into(), json!(ahead));
                map.insert("behind".into(), json!(behind));
            }
        }
        branches.push(row);
    }

    let uncommitted = git_lines(root, &["status", "--porcelain"])
        .map(|lines| lines.iter().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);

    json!({
        "repository": true,
        // The single most useful fact when work has gone missing: a detached
        // position is why a commit "vanished" after a checkout. `repo_status`
        // states it too (#2551) — the point of repeating it here is *when*,
        // not whether: this is the first call of a session.
        "detached": branch.is_none(),
        "branch": branch,
        "default_branch": default_branch,
        "uncommitted_files": uncommitted,
        "shelved": shelved,
        "remotes": remotes.iter().map(|(n, u)| json!({"name": n, "url": u}))
            .collect::<Vec<_>>(),
        "branches": branches,
        "branch_count": branch_total,
        "branch_convention": convention,
        "recent_commits": commits,
        // The routing hint, and this is the first call of a session, so it is
        // the earliest chance to get the ladder right. It named the two deeper
        // rungs and skipped the cheapest one (#2575) — `repo_status` costs a
        // single git call, needs no arguments, and already answers the
        // question the other two are usually reached for.
        "note": "orientation only — `repo_status` is the next step for live state \
                 (ahead/behind, changed files, shelved count, commits no ref points at); \
                 `repo_history` goes deeper (position log, full branch list); \
                 `repo_recover` searches the object store for commits even the reflog \
                 has forgotten",
    })
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
                             entry points and storage are unavailable until then",
                }),
            );
            // Languages without the graph. `code_section` derives them from
            // indexed files, so the one moment a caller most needs to know what
            // kind of tree this is — the first call, before any index exists —
            // was the one moment the field was missing entirely. Tracked files
            // only, via git, so a vendored `node_modules` cannot rename the
            // project's language.
            let languages = tracked_languages(root);
            if !languages.is_empty() {
                map.insert("languages".into(), json!(languages));
            }
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

/// Languages present among git-TRACKED files, for the pre-index case.
///
/// Tracked rather than walked, for the reason the code graph is indexed rather
/// than globbed: a vendored `node_modules` or `target/` dwarfs the project and
/// would rename its language. `git ls-files` already answers "what is actually
/// ours", and honours `.gitignore` for free.
///
/// Reuses [`language_of`] so this fallback and the indexed answer can never
/// disagree about what a `.rs` file is.
fn tracked_languages(root: &Path) -> Vec<&'static str> {
    let Some(files) = git_lines(root, &["ls-files"]) else {
        return Vec::new();
    };
    let mut found: BTreeSet<&'static str> = BTreeSet::new();
    for file in &files {
        if let Some(language) = language_of(file) {
            found.insert(language);
        }
    }
    found.into_iter().collect()
}

fn open_graph(root: &Path) -> Option<crate::graph::OpenedGraph> {
    // Build on first use, the same path `graph_query` takes: project_overview
    // is meant to be the FIRST call in a session, before the background index
    // build could possibly have finished, so it must be able to produce the
    // index it reports on rather than waiting for one to appear.
    crate::graph::open_or_build(root).ok()
}

/// Report the index's size, distinguishing "empty" from "could not be
/// counted".
///
/// Every count here used to be `.unwrap_or(0)` beside a flat `"built": true`,
/// so a workspace whose graph existed but whose counts all failed rendered
/// **identically** to one that was indexed and genuinely held nothing:
/// `built: true, files: 0, symbols: 0, imports: 0`. A real bench trial
/// reported exactly that shape against an `/app` workspace with a committed,
/// non-empty baseline, and the reading was unfalsifiable from the output
/// alone — which is the whole defect in #3087, independent of its cause.
///
/// A failed count is now `null` with the error named, and `built` says which
/// of the three states this is rather than asserting the cheerful one.
fn index_section(graph: &stella_graph::CodeGraph, index_warning: Option<&str>) -> Value {
    index_json(
        graph.file_count().map_err(|e| e.to_string()),
        graph.symbol_count().map_err(|e| e.to_string()),
        graph.import_count().map_err(|e| e.to_string()),
        index_warning,
    )
}

/// The decision half of [`index_section`], with no database in it.
///
/// Split out so the case that matters — a count that *failed* — is reachable
/// from a test at all. Reproducing it through `CodeGraph` would mean
/// corrupting a SQLite file mid-flight, which is why the old shape went
/// unchallenged: nothing could exhibit it.
fn index_json(
    files: Result<usize, String>,
    symbols: Result<usize, String>,
    imports: Result<usize, String>,
    index_warning: Option<&str>,
) -> Value {
    // Each count reports its own truth — a count that succeeded is a fact and
    // nulling it would discard one — while a single failure is enough to
    // retract `built` and name the error. That pairing is what makes the
    // section unambiguous: a number is real, a `null` is unknown, and
    // `built: false` says not to read the section as a description of the
    // workspace.
    let count_error = files
        .as_ref()
        .err()
        .or(symbols.as_ref().err())
        .or(imports.as_ref().err())
        .cloned();

    let mut section = json!({
        // `true` is now a claim about having *read* the index, not merely
        // about having opened it.
        "built": count_error.is_none(),
        "files": files.ok(),
        "symbols": symbols.ok(),
        "imports": imports.ok(),
        // The index is a point-in-time build, so anything written since is
        // invisible here. Saying so is what keeps a stale answer from being
        // mistaken for a current one.
        "freshness": "caught up to the working tree when this call ran",
    });
    if let Some(error) = count_error {
        let map = section.as_object_mut().expect("object literal");
        map.insert(
            "freshness".into(),
            json!("UNKNOWN — the index opened but could not be counted"),
        );
        map.insert("count_error".into(), json!(error));
    }
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

    /// The defect #3087 was reported through: a graph that opens but cannot be
    /// counted must not render identically to one that is genuinely empty.
    ///
    /// Before this, every count was `.unwrap_or(0)` beside a literal
    /// `"built": true`, so both cases produced
    /// `built: true, files: 0, symbols: 0, imports: 0` and no reader could
    /// tell them apart — which is exactly what a real bench trial reported
    /// against an `/app` workspace that git had a non-empty baseline for.
    #[test]
    fn a_countable_and_an_uncountable_index_do_not_render_identically() {
        let empty = index_json(Ok(0), Ok(0), Ok(0), None);
        let broken = index_json(
            Err("no such table: code_graph_files".into()),
            Ok(0),
            Ok(0),
            None,
        );
        assert_ne!(
            empty, broken,
            "an unreadable index and an empty one must be distinguishable"
        );

        assert_eq!(empty["built"], serde_json::json!(true));
        assert_eq!(empty["files"], serde_json::json!(0));

        assert_eq!(
            broken["built"],
            serde_json::json!(false),
            "`built` claims the index was READ, not merely opened"
        );
        assert!(
            broken["files"].is_null(),
            "a count that failed is null, never a confident zero"
        );
        assert_eq!(
            broken["count_error"],
            serde_json::json!("no such table: code_graph_files"),
            "the reason is named, not summarised away"
        );
    }

    /// A partial failure reports exactly what it knows: the counts that
    /// succeeded stay (they are facts, and discarding them would lose real
    /// information), the one that failed is `null`, and `built: false` plus
    /// the named error is what stops the section being read as a description
    /// of the workspace.
    #[test]
    fn a_partial_failure_keeps_the_facts_and_retracts_the_claim() {
        let broken = index_json(Ok(12), Err("disk I/O error".into()), Ok(7), None);
        assert_eq!(broken["files"], serde_json::json!(12));
        assert_eq!(broken["imports"], serde_json::json!(7));
        assert!(
            broken["symbols"].is_null(),
            "only the count that failed is unknown"
        );
        assert_eq!(
            broken["built"],
            serde_json::json!(false),
            "one failure is still enough to retract the claim that the index was read"
        );
        assert_eq!(broken["count_error"], serde_json::json!("disk I/O error"));
    }

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

    /// A tree under no version control says so *in the assembled payload*,
    /// rather than omitting the section and leaving the reader to guess
    /// whether it was forgotten. The sibling
    /// `a_non_repository_says_so_rather_than_erroring` pins the same fact at
    /// the section function; this one pins that the section reaches the
    /// caller at all.
    #[test]
    fn a_non_repository_says_so_in_the_assembled_overview() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn f() {}\n").unwrap();
        assert_eq!(
            build_overview(dir.path())["git"],
            json!({ "repository": false })
        );
    }

    /// Witness: the first call of a session must be able to say that the
    /// working position is DETACHED, which is the answer to a large share of
    /// "my changes are gone after a checkout". Before this the `git` section
    /// named the remotes and the branch count and could not say where the
    /// position *was* — and a read-only role has no `bash` to ask with.
    #[test]
    fn the_overview_reports_a_detached_position_and_recent_commits() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let mut c = std::process::Command::new("git");
            c.args(args).current_dir(dir.path());
            crate::subprocess_env::scrub_spawn_std_env(&mut c);
            c.output().expect("git must be installed for this test")
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "T"]);
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "first commit"]);

        let attached = build_overview(dir.path());
        let repo = &attached["git"];
        assert_eq!(repo["repository"], json!(true), "{repo}");
        assert_eq!(repo["detached"], json!(false), "{repo}");
        assert_eq!(repo["branch"], json!("main"), "{repo}");
        assert!(
            repo["recent_commits"].as_array().is_some_and(|c| c
                .iter()
                .any(|l| l.as_str().is_some_and(|s| s.contains("first commit")))),
            "the last commits must be in the overview: {repo}"
        );

        // Detach exactly the way the bench task's user did.
        let head = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_string();
        git(&["checkout", "-q", &head]);
        let detached = build_overview(dir.path());
        assert_eq!(detached["git"]["detached"], json!(true));
        assert_eq!(detached["git"]["branch"], json!(null));
    }

    /// Uncommitted and shelved work are both counted — the two states a
    /// "where did my change go?" question resolves to most often.
    #[test]
    fn the_overview_counts_uncommitted_and_shelved_changes() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let mut c = std::process::Command::new("git");
            c.args(args).current_dir(dir.path());
            crate::subprocess_env::scrub_spawn_std_env(&mut c);
            c.output().expect("git must be installed for this test")
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "T"]);
        // `build_overview` builds the code graph under `.stella/`, which is
        // then a legitimately untracked path. Ignore it so this test counts
        // the fixture's own changes and not Stella's working state.
        std::fs::write(dir.path().join(".gitignore"), ".stella/\n").unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        git(&["add", "a.txt", ".gitignore"]);
        git(&["commit", "-q", "-m", "first"]);

        std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        assert_eq!(
            build_overview(dir.path())["git"]["uncommitted_files"],
            json!(1)
        );

        git(&["stash", "-q"]);
        let repo = &build_overview(dir.path())["git"];
        assert_eq!(repo["uncommitted_files"], json!(0), "{repo}");
        assert_eq!(
            repo["shelved"].as_array().map(Vec::len),
            Some(1),
            "a stashed change must be visible without `bash`: {repo}"
        );
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

    /// An ambient `GIT_DIR` must not re-aim the overview at another repository.
    ///
    /// `current_dir` does not decide which repo git reads — `GIT_DIR` overrides
    /// it, and git EXPORTS `GIT_DIR` to every hook it runs. So an overview
    /// assembled from inside a pre-push hook would report the outer
    /// repository's branches and remotes as this workspace's, with no error to
    /// notice. The failure is silent and the answer is confidently wrong, which
    /// is the worst combination a orientation tool can have.
    ///
    /// Set on the *process*, because that is how a hook delivers it.
    #[test]
    fn an_ambient_git_dir_cannot_retarget_the_overview() {
        // Serialised against nothing else: this mutates process env, so it must
        // restore it before returning even on failure.
        let elsewhere = tempfile::tempdir().unwrap();
        let here = tempfile::tempdir().unwrap();

        let init = |dir: &Path, remote: &str| {
            for args in [
                vec!["init", "--quiet"],
                vec!["remote", "add", "origin", remote],
            ] {
                let _ = std::process::Command::new("git")
                    .args(&args)
                    .current_dir(dir)
                    .output();
            }
        };
        init(elsewhere.path(), "https://example.invalid/OUTER.git");
        init(here.path(), "https://example.invalid/INNER.git");

        let previous = std::env::var_os("GIT_DIR");
        // SAFETY: single-threaded test body; restored below before returning.
        unsafe { std::env::set_var("GIT_DIR", elsewhere.path().join(".git")) };
        let section = git_section(here.path());
        match previous {
            Some(value) => unsafe { std::env::set_var("GIT_DIR", value) },
            None => unsafe { std::env::remove_var("GIT_DIR") },
        }

        let rendered = section.to_string();
        assert!(
            !rendered.contains("OUTER"),
            "an ambient GIT_DIR re-aimed the overview at another repository: {rendered}"
        );
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
