//! `stella stats graph` — code-graph retrieval health, and the before/after
//! comparison that lets a claimed rise in graph-backed retrieval be checked
//! rather than asserted (#896).
//!
//! Reads what `stella-store` recorded and nothing else: no provider is
//! resolved, no API key is needed, and no file is created just to read it.
//!
//! # The two halves of the report, and why both are needed
//!
//! * **adoption** — is `graph_query` reached for at all, next to the text
//!   tools it competes with. This is the one telemetry showed near zero.
//! * **resolved-query rate** — of the queries that *were* made, what share
//!   came back with frames rather than a miss. This is the health metric:
//!   a tool that answers "nothing found" teaches the model to stop asking,
//!   so a rising adoption number on top of a falling resolve rate is a
//!   regression wearing a win's clothes.
//!
//! Classification comes from `stella_tools::graph::classify_answer`, which
//! owns the exact literals every miss branch emits — this module never
//! pattern-matches prose of its own.
//!
//! # Why this module also compares
//!
//! #896's acceptance clause is a *measurement* ("a measurable rise in
//! graph-backed retrieval"), not a diff. A point-in-time number cannot
//! satisfy it: "up" is a claim about two observations. Three capabilities
//! turn the number into evidence, and all three live here:
//!
//! * `--workspace` reads a store that is not the current directory, so an
//!   archived pre-change workspace can be measured at all.
//! * `--scan` pools every workspace under a directory into one report, so a
//!   benchmark run — which is N task workspaces, each with its own store —
//!   yields a single run-level number instead of N to sum by hand.
//! * `--save-baseline` / `--baseline` persist an observation and diff
//!   against it, with a verdict that encodes the disguised-regression trap
//!   above rather than leaving two tables to be eyeballed.
//!
//! The instrument is deliberately not the measurement. Nothing here asserts
//! that adoption rose; it makes the question answerable from recorded
//! telemetry, and answerable the same way twice.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::query_format::{StatsFormat, Versioned};
use stella_store::Store;

/// Flags for `stella stats graph`.
#[derive(clap::Args, Debug, Clone)]
pub struct GraphArgs {
    /// Output format: text (aligned table, default), json under the
    /// versioned query envelope, or csv
    #[arg(long, value_enum, default_value = "text")]
    pub format: StatsFormat,

    /// How many recent graph_query answers to classify, per workspace
    #[arg(long, value_name = "N", default_value_t = 500)]
    pub limit: usize,

    /// Read this workspace instead of the current directory. Repeatable —
    /// every named workspace is pooled into one report
    #[arg(long, value_name = "DIR")]
    pub workspace: Vec<PathBuf>,

    /// Find every workspace under DIR and pool them into one report — the
    /// run-level number for a benchmark sweep or a tree of checkouts
    #[arg(long, value_name = "DIR", conflicts_with = "workspace")]
    pub scan: Option<PathBuf>,

    /// How deep --scan descends looking for workspaces
    #[arg(long, value_name = "N", default_value_t = DEFAULT_SCAN_DEPTH)]
    pub scan_depth: usize,

    /// Write this report to FILE as a baseline to compare a later run against
    #[arg(long, value_name = "FILE")]
    pub save_baseline: Option<PathBuf>,

    /// Note recorded in the saved baseline — what this observation is of
    /// (e.g. a commit, a benchmark run id)
    #[arg(long, value_name = "TEXT", requires = "save_baseline")]
    pub label: Option<String>,

    /// Compare this report against a baseline saved earlier and report the
    /// deltas with a verdict
    #[arg(long, value_name = "FILE")]
    pub baseline: Option<PathBuf>,

    /// Exit non-zero when the comparison verdict is a regression, so a
    /// benchmark or CI job can gate on it. Off by default: reporting a
    /// regression and failing on one are different requests
    #[arg(long, requires = "baseline")]
    pub fail_on_regression: bool,
}

/// Tool names counted as the text-search alternatives to the code graph, for
/// the adoption denominator. `bash` is deliberately absent: most bash calls
/// are builds and git, not searches, so folding it in would drown the ratio.
const TEXT_SEARCH_TOOLS: &[&str] = &["grep", "glob"];

/// How deep `--scan` descends by default. Four levels reaches
/// `<runs>/<run-id>/<task-id>/<checkout>` — the shape a benchmark sweep
/// leaves behind — without walking an entire home directory.
const DEFAULT_SCAN_DEPTH: usize = 4;

/// Directory names `--scan` never descends into: large, never workspaces,
/// and (for `target`) full of vendored checkouts that would be counted as if
/// the agent had worked in them.
const SCAN_SKIP_DIRS: &[&str] = &["node_modules", "target", "vendor", "dist", "build"];

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

/// One `op`'s answer breakdown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphOpRow {
    pub op: String,
    pub calls: u64,
    pub resolved: u64,
    pub unresolved: u64,
    pub out_of_root: u64,
    pub not_indexed: u64,
    /// Calls that came back as `ToolOutput::Error` — a broken query, not an
    /// empty one. Excluded from `resolved` and reported separately so a store
    /// fault cannot masquerade as a retrieval miss.
    pub failed: u64,
    /// `resolved / calls`, or 0.0 for an op with no calls.
    pub resolved_rate: f64,
}

impl GraphOpRow {
    fn new(op: &str) -> Self {
        Self {
            op: op.to_string(),
            calls: 0,
            resolved: 0,
            unresolved: 0,
            out_of_root: 0,
            not_indexed: 0,
            failed: 0,
            resolved_rate: 0.0,
        }
    }

    fn finish(&mut self) {
        self.resolved_rate = if self.calls == 0 {
            0.0
        } else {
            self.resolved as f64 / self.calls as f64
        };
    }

    /// Fold another row's counts in. Rates are derived, so the caller
    /// re-runs [`GraphOpRow::finish`] once the summing is done.
    fn absorb(&mut self, other: &GraphOpRow) {
        self.calls += other.calls;
        self.resolved += other.resolved;
        self.unresolved += other.unresolved;
        self.out_of_root += other.out_of_root;
        self.not_indexed += other.not_indexed;
        self.failed += other.failed;
    }
}

/// The whole report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphHealth {
    /// Per-op rows, busiest first.
    pub ops: Vec<GraphOpRow>,
    /// The same counts summed — the headline resolved-query rate.
    pub total: GraphOpRow,
    /// Lifetime `graph_query` call count from the projection (not the
    /// classified window, which is capped by `--limit`).
    pub graph_query_calls: i64,
    /// Lifetime calls of the text-search tools it competes with.
    pub text_search_calls: i64,
    /// `graph_query / (graph_query + text search)`.
    pub adoption_rate: f64,
    /// How many workspace stores this report pools. Part of the measurement's
    /// provenance, not decoration: a one-workspace baseline against a
    /// sixty-workspace run is not a like-for-like comparison, and the
    /// comparison renderer says so.
    #[serde(default)]
    pub workspaces: usize,
}

impl GraphHealth {
    /// A report over nothing — what a workspace with no store yet yields.
    fn empty() -> Self {
        Self {
            ops: Vec::new(),
            total: GraphOpRow::new("TOTAL"),
            graph_query_calls: 0,
            text_search_calls: 0,
            adoption_rate: 0.0,
            workspaces: 0,
        }
    }

    /// Calls of every tool in the adoption ratio — the denominator, and the
    /// sample size the verdict's confidence bar is read against.
    pub fn structural_searches(&self) -> i64 {
        self.graph_query_calls + self.text_search_calls
    }
}

/// Fold classified answers plus lifetime call counts into the report. Pure, so
/// the arithmetic is testable without a store.
fn graph_health(answers: &[stella_store::ToolCallAnswer], counts: &[(String, i64)]) -> GraphHealth {
    use stella_tools::graph::{GraphAnswer, classify_answer};

    let mut by_op: HashMap<String, GraphOpRow> = HashMap::new();
    let mut total = GraphOpRow::new("TOTAL");
    for answer in answers {
        let op = serde_json::from_str::<serde_json::Value>(&answer.args_json)
            .ok()
            .and_then(|v| v.get("op").and_then(|o| o.as_str()).map(str::to_string))
            .unwrap_or_else(|| "(unknown)".to_string());
        let row = by_op
            .entry(op.clone())
            .or_insert_with(|| GraphOpRow::new(&op));
        for target in [row, &mut total] {
            target.calls += 1;
            if !answer.ok {
                target.failed += 1;
                continue;
            }
            match classify_answer(&answer.content) {
                GraphAnswer::Resolved => target.resolved += 1,
                GraphAnswer::Unresolved => target.unresolved += 1,
                GraphAnswer::OutOfRoot => target.out_of_root += 1,
                GraphAnswer::NotIndexed => target.not_indexed += 1,
            }
        }
    }
    let mut ops: Vec<GraphOpRow> = by_op.into_values().collect();
    for row in &mut ops {
        row.finish();
    }
    ops.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.op.cmp(&b.op)));
    total.finish();

    let lookup = |name: &str| {
        counts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, c)| *c)
            .unwrap_or(0)
    };
    let graph_query_calls = lookup("graph_query");
    let text_search_calls: i64 = TEXT_SEARCH_TOOLS.iter().map(|n| lookup(n)).sum();
    let denominator = graph_query_calls + text_search_calls;
    GraphHealth {
        ops,
        total,
        graph_query_calls,
        text_search_calls,
        adoption_rate: if denominator == 0 {
            0.0
        } else {
            graph_query_calls as f64 / denominator as f64
        },
        workspaces: 1,
    }
}

/// Pool per-workspace reports into one.
///
/// Counts sum and every rate is re-derived from the summed counts, which
/// makes the pooled figure call-weighted rather than an average of averages:
/// a workspace with three queries cannot swing the run-level rate as far as
/// one with three hundred. That is the right weighting for "how did the tool
/// do across this run", and the wrong one for "was any single workspace
/// broken" — for the latter, report the workspaces separately.
fn merge_health(parts: &[GraphHealth]) -> GraphHealth {
    let mut by_op: HashMap<String, GraphOpRow> = HashMap::new();
    let mut merged = GraphHealth::empty();
    for part in parts {
        for row in &part.ops {
            by_op
                .entry(row.op.clone())
                .or_insert_with(|| GraphOpRow::new(&row.op))
                .absorb(row);
        }
        merged.total.absorb(&part.total);
        merged.graph_query_calls += part.graph_query_calls;
        merged.text_search_calls += part.text_search_calls;
        merged.workspaces += part.workspaces;
    }
    let mut ops: Vec<GraphOpRow> = by_op.into_values().collect();
    for row in &mut ops {
        row.finish();
    }
    ops.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.op.cmp(&b.op)));
    merged.total.finish();
    merged.ops = ops;
    let denominator = merged.structural_searches();
    merged.adoption_rate = if denominator == 0 {
        0.0
    } else {
        merged.graph_query_calls as f64 / denominator as f64
    };
    merged
}

// ---------------------------------------------------------------------------
// Baselines and the comparison
// ---------------------------------------------------------------------------

/// Snapshot format version. Bumped only when a reader would misread an older
/// file; additive fields carry `#[serde(default)]` instead.
pub const GRAPH_SNAPSHOT_SCHEMA: u32 = 1;

/// A report persisted for later comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphSnapshot {
    pub schema: u32,
    /// What this observation is of — a commit, a benchmark run id, "before
    /// #1247". Free text, because provenance a tool can validate is
    /// provenance a tool can also get confidently wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Seconds since the Unix epoch, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_at_unix: Option<u64>,
    pub health: GraphHealth,
}

/// Below this many structural searches on either side, no verdict is
/// rendered.
///
/// Adoption is a ratio whose denominator is every grep, glob and
/// `graph_query` in the window. Over a handful of searches a single extra
/// call moves it by tens of points, so a "rise" measured on six calls is
/// noise with a percentage sign. Refusing to grade a small sample is the
/// difference between an instrument and a rubber stamp: this issue can only
/// be closed on evidence, and evidence that would flip on one more grep is
/// not evidence.
const MIN_STRUCTURAL_SEARCHES: i64 = 30;

/// Adoption changes smaller than this (in rate, so 0.01 = one percentage
/// point) read as unchanged rather than as movement.
const ADOPTION_NOISE_FLOOR: f64 = 0.01;

/// A resolved-query-rate drop steeper than this (five percentage points)
/// disqualifies an adoption rise from counting as a win.
///
/// This is the disguised regression the module docs name: queries the model
/// makes more often but that land less often is the tool getting *worse* at
/// the job the adoption number is being read as evidence for.
const RESOLVE_REGRESSION_TOLERANCE: f64 = 0.05;

/// What the comparison concluded. Each variant names a different next action,
/// which is why the disguised regression is not folded into `Regressed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphVerdict {
    /// Too few structural searches on one side to grade.
    Inconclusive,
    /// Adoption fell materially.
    Regressed,
    /// Adoption held or rose, but the resolved-query rate fell beyond
    /// tolerance — the queries are being made and are not landing.
    ResolveRegressed,
    /// Adoption moved less than the noise floor.
    Unchanged,
    /// Adoption rose materially and the resolve rate held.
    Improved,
}

impl GraphVerdict {
    /// Whether this verdict should fail a job that opted into gating.
    pub fn is_regression(self) -> bool {
        matches!(
            self,
            GraphVerdict::Regressed | GraphVerdict::ResolveRegressed
        )
    }

    fn as_str(self) -> &'static str {
        match self {
            GraphVerdict::Inconclusive => "INCONCLUSIVE",
            GraphVerdict::Regressed => "REGRESSED",
            GraphVerdict::ResolveRegressed => "RESOLVE-REGRESSED",
            GraphVerdict::Unchanged => "UNCHANGED",
            GraphVerdict::Improved => "IMPROVED",
        }
    }
}

/// One metric's before, after, and difference.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GraphDelta {
    pub baseline: f64,
    pub current: f64,
    pub delta: f64,
}

impl GraphDelta {
    fn new(baseline: f64, current: f64) -> Self {
        Self {
            baseline,
            current,
            delta: current - baseline,
        }
    }
}

/// A baseline and a current report, judged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphComparison {
    pub verdict: GraphVerdict,
    /// Why the verdict came out that way, in a sentence — so the number in a
    /// CI log explains itself without the reader knowing the thresholds.
    pub rationale: String,
    pub adoption: GraphDelta,
    pub resolved_rate: GraphDelta,
    pub baseline_searches: i64,
    pub current_searches: i64,
    pub baseline_workspaces: usize,
    pub current_workspaces: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_label: Option<String>,
}

/// Verifier a current report against a baseline.
///
/// Pure: every threshold is a named constant above and no I/O happens here,
/// so the grading rule is testable and reviewable as one function.
fn compare(baseline: &GraphSnapshot, current: &GraphHealth) -> GraphComparison {
    let base = &baseline.health;
    let adoption = GraphDelta::new(base.adoption_rate, current.adoption_rate);
    let resolved_rate = GraphDelta::new(base.total.resolved_rate, current.total.resolved_rate);
    let baseline_searches = base.structural_searches();
    let current_searches = current.structural_searches();

    let (verdict, rationale) = if baseline_searches < MIN_STRUCTURAL_SEARCHES
        || current_searches < MIN_STRUCTURAL_SEARCHES
    {
        (
            GraphVerdict::Inconclusive,
            format!(
                "fewer than {MIN_STRUCTURAL_SEARCHES} structural searches on one side \
                 (baseline {baseline_searches}, current {current_searches}) — an adoption \
                 ratio over this few calls moves by tens of points on one more grep, so \
                 there is nothing here to grade"
            ),
        )
    } else if adoption.delta <= -ADOPTION_NOISE_FLOOR {
        (
            GraphVerdict::Regressed,
            format!(
                "adoption fell {:.1} points ({:.1}% → {:.1}%)",
                -adoption.delta * 100.0,
                adoption.baseline * 100.0,
                adoption.current * 100.0
            ),
        )
    } else if resolved_rate.delta < -RESOLVE_REGRESSION_TOLERANCE {
        (
            GraphVerdict::ResolveRegressed,
            format!(
                "the resolved-query rate fell {:.1} points ({:.1}% → {:.1}%), which \
                 disqualifies the adoption move: queries made more often but landing \
                 less often is the tool getting worse at the job adoption is read as \
                 evidence for",
                -resolved_rate.delta * 100.0,
                resolved_rate.baseline * 100.0,
                resolved_rate.current * 100.0
            ),
        )
    } else if adoption.delta >= ADOPTION_NOISE_FLOOR {
        (
            GraphVerdict::Improved,
            format!(
                "adoption rose {:.1} points ({:.1}% → {:.1}%) and the resolved-query \
                 rate held",
                adoption.delta * 100.0,
                adoption.baseline * 100.0,
                adoption.current * 100.0
            ),
        )
    } else {
        (
            GraphVerdict::Unchanged,
            format!(
                "adoption moved {:.1} points, under the {:.0}-point noise floor",
                adoption.delta * 100.0,
                ADOPTION_NOISE_FLOOR * 100.0
            ),
        )
    };

    GraphComparison {
        verdict,
        rationale,
        adoption,
        resolved_rate,
        baseline_searches,
        current_searches,
        baseline_workspaces: base.workspaces,
        current_workspaces: current.workspaces,
        baseline_label: baseline.label.clone(),
    }
}

// ---------------------------------------------------------------------------
// Finding the stores to read
// ---------------------------------------------------------------------------

/// Whether a directory is a workspace with telemetry to read.
fn has_store(root: &Path) -> bool {
    matches!(
        stella_store::existing_workspace_private_sqlite_path(root, "store.db"),
        Ok(Some(_))
    )
}

/// Find every workspace under `root`, breadth-first, to `max_depth` levels.
///
/// A directory that *is* a workspace is not descended into: its
/// subdirectories are its own source tree, not sibling workspaces. Symlinks
/// are never followed, so a link back up the tree cannot make the walk
/// unbounded or count one store twice.
fn discover_workspaces(root: &Path, max_depth: usize) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    let mut frontier = vec![root.to_path_buf()];
    for _ in 0..=max_depth {
        if frontier.is_empty() {
            break;
        }
        let mut next = Vec::new();
        for dir in frontier {
            if has_store(&dir) {
                found.push(dir);
                continue;
            }
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                // An unreadable directory is skipped, not fatal: scanning a
                // tree that happens to contain one root-owned subdirectory
                // should still report on everything else in it.
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') || SCAN_SKIP_DIRS.contains(&name.as_ref()) {
                    continue;
                }
                // `file_type` on a `DirEntry` does not traverse symlinks.
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    next.push(entry.path());
                }
            }
        }
        frontier = next;
    }
    found.sort();
    Ok(found)
}

/// Read one workspace's health, or the empty report when it has no store yet.
fn health_for(root: &Path, limit: usize) -> Result<GraphHealth, String> {
    if !has_store(root) {
        return Ok(GraphHealth::empty());
    }
    let store = Store::open(root).map_err(|e| format!("cannot open store in {root:?}: {e}"))?;
    let answers = store
        .tool_call_answers("graph_query", limit)
        .map_err(|e| format!("cannot read graph_query answers in {root:?}: {e}"))?;
    let counts = store
        .tool_call_counts()
        .map_err(|e| format!("cannot read tool call counts in {root:?}: {e}"))?;
    Ok(graph_health(&answers, &counts))
}

/// Which workspaces this invocation reports on.
fn resolve_roots(args: &GraphArgs) -> Result<Vec<PathBuf>, String> {
    if !args.workspace.is_empty() {
        // An explicitly named workspace with no store is a typo, not an
        // empty result: reporting zeroes for a path the user misspelled is
        // the failure mode that makes a measurement quietly meaningless.
        for root in &args.workspace {
            if !root.is_dir() {
                return Err(format!("not a directory: {}", root.display()));
            }
            if !has_store(root) {
                return Err(format!(
                    "no telemetry in {} — .stella/private/store.db does not exist there",
                    root.display()
                ));
            }
        }
        return Ok(args.workspace.clone());
    }
    if let Some(scan) = &args.scan {
        if !scan.is_dir() {
            return Err(format!("not a directory: {}", scan.display()));
        }
        let found = discover_workspaces(scan, args.scan_depth)?;
        if found.is_empty() {
            return Err(format!(
                "no workspaces with telemetry under {} (depth {}) — nothing to report on",
                scan.display(),
                args.scan_depth
            ));
        }
        return Ok(found);
    }
    Ok(vec![std::env::current_dir().map_err(|e| {
        format!("cannot determine workspace root: {e}")
    })?])
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_graph_table(health: &GraphHealth) -> String {
    let mut out = String::from("graph_query retrieval health (from .stella/private/store.db)\n");
    if health.workspaces != 1 {
        out.push_str(&format!(
            "pooled over {} workspaces — counts summed, every rate re-derived from the \
             summed counts\n",
            health.workspaces
        ));
    }
    out.push('\n');
    out.push_str(&format!(
        "{:<14} {:>6} {:>9} {:>11} {:>12} {:>12} {:>7} {:>10}\n",
        "OP",
        "CALLS",
        "RESOLVED",
        "UNRESOLVED",
        "OUT-OF-ROOT",
        "NOT-INDEXED",
        "FAILED",
        "RESOLVED%"
    ));
    let line = |row: &GraphOpRow| {
        format!(
            "{:<14} {:>6} {:>9} {:>11} {:>12} {:>12} {:>7} {:>9.1}%\n",
            row.op,
            row.calls,
            row.resolved,
            row.unresolved,
            row.out_of_root,
            row.not_indexed,
            row.failed,
            row.resolved_rate * 100.0
        )
    };
    for row in &health.ops {
        out.push_str(&line(row));
    }
    out.push_str(&line(&health.total));
    out.push('\n');
    out.push_str(&format!(
        "adoption: graph_query {} of {} structural searches (graph_query + {}) = {:.1}%\n",
        health.graph_query_calls,
        health.structural_searches(),
        TEXT_SEARCH_TOOLS.join(" + "),
        health.adoption_rate * 100.0
    ));
    out.push_str(
        "out-of-root and not-indexed are answers re-indexing cannot fix — a session rooted \
         on the wrong tree, or a file type the graph does not carry.\n",
    );
    out
}

fn render_graph_csv(health: &GraphHealth) -> String {
    let mut out =
        String::from("op,calls,resolved,unresolved,out_of_root,not_indexed,failed,resolved_rate\n");
    for row in health.ops.iter().chain(std::iter::once(&health.total)) {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{:.4}\n",
            row.op,
            row.calls,
            row.resolved,
            row.unresolved,
            row.out_of_root,
            row.not_indexed,
            row.failed,
            row.resolved_rate
        ));
    }
    out
}

fn render_comparison_table(cmp: &GraphComparison) -> String {
    let mut out = String::from("graph_query retrieval: current vs baseline\n\n");
    out.push_str(&format!(
        "{:<22} {:>10} {:>10} {:>10}\n",
        "METRIC", "BASELINE", "CURRENT", "DELTA"
    ));
    let line = |name: &str, d: &GraphDelta| {
        format!(
            "{:<22} {:>9.1}% {:>9.1}% {:>+9.1}pp\n",
            name,
            d.baseline * 100.0,
            d.current * 100.0,
            d.delta * 100.0
        )
    };
    out.push_str(&line("adoption", &cmp.adoption));
    out.push_str(&line("resolved-query rate", &cmp.resolved_rate));
    out.push_str(&format!(
        "{:<22} {:>10} {:>10} {:>+10}\n",
        "structural searches",
        cmp.baseline_searches,
        cmp.current_searches,
        cmp.current_searches - cmp.baseline_searches
    ));
    out.push_str(&format!(
        "{:<22} {:>10} {:>10}\n",
        "workspaces pooled", cmp.baseline_workspaces, cmp.current_workspaces
    ));
    out.push('\n');
    if let Some(label) = &cmp.baseline_label {
        out.push_str(&format!("baseline: {label}\n"));
    }
    out.push_str(&format!("{}: {}\n", cmp.verdict.as_str(), cmp.rationale));
    out
}

fn render_comparison_csv(cmp: &GraphComparison) -> String {
    let mut out = String::from("metric,baseline,current,delta\n");
    out.push_str(&format!(
        "adoption_rate,{:.4},{:.4},{:.4}\n",
        cmp.adoption.baseline, cmp.adoption.current, cmp.adoption.delta
    ));
    out.push_str(&format!(
        "resolved_rate,{:.4},{:.4},{:.4}\n",
        cmp.resolved_rate.baseline, cmp.resolved_rate.current, cmp.resolved_rate.delta
    ));
    out.push_str(&format!(
        "structural_searches,{},{},{}\n",
        cmp.baseline_searches,
        cmp.current_searches,
        cmp.current_searches - cmp.baseline_searches
    ));
    out.push_str(&format!("verdict,,{},\n", cmp.verdict.as_str()));
    out
}

// ---------------------------------------------------------------------------
// Baseline I/O
// ---------------------------------------------------------------------------

fn read_baseline(path: &Path) -> Result<GraphSnapshot, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read baseline {}: {e}", path.display()))?;
    let snapshot: GraphSnapshot = serde_json::from_str(&text)
        .map_err(|e| format!("baseline {} is not a graph snapshot: {e}", path.display()))?;
    if snapshot.schema > GRAPH_SNAPSHOT_SCHEMA {
        return Err(format!(
            "baseline {} is schema v{} but this build reads up to v{} — it was written by a \
             newer stella",
            path.display(),
            snapshot.schema,
            GRAPH_SNAPSHOT_SCHEMA
        ));
    }
    Ok(snapshot)
}

fn write_baseline(
    path: &Path,
    health: &GraphHealth,
    label: Option<&str>,
    now_unix: Option<u64>,
) -> Result<(), String> {
    let snapshot = GraphSnapshot {
        schema: GRAPH_SNAPSHOT_SCHEMA,
        label: label.map(str::to_string),
        captured_at_unix: now_unix,
        health: health.clone(),
    };
    let text = serde_json::to_string_pretty(&snapshot).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// `1 workspace` / `2 workspaces` — the counts in this report are read by
/// people deciding whether two observations are comparable, so the sentence
/// should not make them stop and parse it.
fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn now_unix() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Entry point for `stella stats graph`. Read-only against telemetry, keyless,
/// and — like the main report — refuses to create `.stella/` just to read it.
/// The only file it ever writes is an explicitly requested `--save-baseline`.
pub fn run_stats_graph(args: &GraphArgs) -> Result<(), String> {
    let roots = resolve_roots(args)?;
    let mut parts = Vec::with_capacity(roots.len());
    for root in &roots {
        parts.push(health_for(root, args.limit)?);
    }
    let health = merge_health(&parts);

    if let Some(path) = &args.save_baseline {
        write_baseline(path, &health, args.label.as_deref(), now_unix())?;
        eprintln!(
            "stella: baseline written to {} ({}, {} structural searches)",
            path.display(),
            plural(health.workspaces, "workspace"),
            health.structural_searches()
        );
    }

    let Some(baseline_path) = &args.baseline else {
        return render_report(args.format, &health);
    };

    let baseline = read_baseline(baseline_path)?;
    let cmp = compare(&baseline, &health);
    match args.format {
        StatsFormat::Text => print!("{}", render_comparison_table(&cmp)),
        StatsFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&Versioned::new(&cmp)).map_err(|e| e.to_string())?
        ),
        StatsFormat::Csv => print!("{}", render_comparison_csv(&cmp)),
    }
    if args.fail_on_regression && cmp.verdict.is_regression() {
        return Err(format!(
            "graph retrieval {}: {}",
            cmp.verdict.as_str().to_lowercase(),
            cmp.rationale
        ));
    }
    Ok(())
}

fn render_report(format: StatsFormat, health: &GraphHealth) -> Result<(), String> {
    match format {
        StatsFormat::Text => {
            if health.total.calls == 0 {
                println!(
                    "no graph_query calls recorded in {} yet — adoption is {} of {} \
                     structural searches",
                    if health.workspaces == 1 {
                        "this workspace".to_string()
                    } else {
                        plural(health.workspaces, "workspace")
                    },
                    health.graph_query_calls,
                    health.structural_searches()
                );
                return Ok(());
            }
            print!("{}", render_graph_table(health));
        }
        StatsFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&Versioned::new(health)).map_err(|e| e.to_string())?
        ),
        StatsFormat::Csv => print!("{}", render_graph_csv(health)),
    }
    Ok(())
}

#[cfg(test)]
mod tests;
